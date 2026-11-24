//! Phones and the group roster (migration step 7): received `roster_intro`s and the offline wake.
//!
//! A phone links over the relay only, and it always says hello first: hubs never dial or hello a phone
//! (`dial_loop` skips them), because every frame queued for an offline phone makes the relay push to
//! it. `wake_phone` is the one deliberate exception.

use super::*;
use crate::intro::{check_intro_device, MAX_INTRO_DEVICES};

/// At most one offline wake per phone this often.
pub(super) const WAKE_EVERY: Duration = Duration::from_secs(60);

impl Inner {
    /// A `roster_intro` that arrived on the live link of `from`: report the devices that pass the
    /// structural checks (the hub decides about the rest). Only a paired hub may introduce devices.
    pub(super) fn roster_intro(&self, from: &str, devices: Vec<IntroDevice>) {
        let Some(sender) = self.roster.get(from) else { return };
        if sender.device.kind != DeviceKind::Hub {
            warn!(device = from, "dropped a roster_intro from a phone");
            return;
        }
        if devices.len() > MAX_INTRO_DEVICES {
            warn!(device = from, count = devices.len(), "dropped a roster_intro with too many devices");
            return;
        }
        // A second layer under the hub's checks: never ourselves, never the sender, each id and key once.
        let (me_id, my_pk) = (self.me_id(), self.keys.public_key());
        let sender_pk = parse_pk(&sender.device.public_key);
        let (mut ids, mut keys) = (HashSet::new(), HashSet::new());
        let (mut valid, mut invalid) = (Vec::new(), Vec::new());
        for device in devices {
            // Both inserts run: a repeat of either the id or the key is a duplicate.
            let first = ids.insert(device.id.clone()) & keys.insert(device.public_key.clone());
            let verdict = check_intro_device(&device).and_then(|pk| {
                if device.id == me_id || pk == my_pk {
                    Err("own_device")
                } else if device.id == from || Some(pk) == sender_pk {
                    Err("sender_device")
                } else if !first {
                    Err("duplicate")
                } else {
                    Ok(())
                }
            });
            match verdict {
                Ok(()) => valid.push(device),
                Err(why) => invalid.push((device, why)),
            }
        }
        info!(device = from, valid = valid.len(), invalid = invalid.len(), "roster intro received");
        self.emit(PeerEvent::RosterIntro { from: from.to_string(), devices: valid, invalid });
    }

    /// Queue one presence for a paired phone that has no link, in its relay mailbox (the relay then
    /// sends the phone a content-free push). Sealed like any frame, with the next tx seq.
    pub(super) fn wake_phone(&self, device_id: &str) -> Result<(), WakeError> {
        // Under the gate, like revoke: a revoke either finished first (the phone is no longer in the
        // roster) or waits until this frame is queued.
        let _g = lock(&self.gate);
        let entry = self.roster.get(device_id).filter(|e| e.device.kind == DeviceKind::Phone).ok_or(WakeError::NotPhone)?;
        let pk = parse_pk(&entry.device.public_key).ok_or(WakeError::NotPhone)?;
        if self.cooldown_until(device_id).is_some() {
            return Err(WakeError::Disconnected);
        }
        if lock(&self.links).contains_key(device_id) {
            return Err(WakeError::Linked);
        }
        let conn = self.relay_conn().filter(|_| !self.relay_restricted()).ok_or(WakeError::RelayOff)?;
        {
            let mut wakes = lock(&self.wakes);
            let now = Instant::now();
            wakes.retain(|_, at| now.duration_since(*at) < WAKE_EVERY);
            if wakes.contains_key(device_id) {
                return Err(WakeError::RateLimited);
            }
            wakes.insert(device_id.to_string(), now);
        }
        let env = seal(&self.keys, &self.me_id(), device_id, &pk, self.next_tx_seq(device_id), &self.presence_body(), false);
        if conn.send_frame(&RelayClientFrame::Send { to: address(&pk), env, pairing: None }).is_err() {
            // Nothing was queued: the next request may try again.
            lock(&self.wakes).remove(device_id);
            return Err(WakeError::Backpressure);
        }
        debug!(device = device_id, "offline wake queued in the phone's relay mailbox");
        Ok(())
    }
}
