// Revoke a paired device; for a hub that had introduced devices here (group roster, step 7), then ask in-app whether
// to remove those too, and revoke each one on yes.
import { useCallback } from "react";
import { introducedByRevoked, revokeIntroducedPrompt, revokeManyText } from "@krypto/hub-client";
import { useConfirm } from "./Confirm";
import { usePanel } from "./context";

/**
 * `revoke(device)` unpairs it (throws the hub's error, for the row to show), toasts, and follows up when the ack lists
 * devices it introduced that are still paired: "Also remove the devices <name> added?". The follow-up runs even
 * after the row is gone (the dialog lives in ConfirmProvider).
 */
export function useRevokeDevice(): (device: { id: string; name: string }) => Promise<void> {
  const { client, toast } = usePanel();
  const confirm = useConfirm();
  return useCallback(
    async (device) => {
      const res = await client.revokeDevice(device.id);
      toast(`Unpaired ${device.name}`);
      const introduced = introducedByRevoked(res, device.id);
      if (introduced.length === 0) return;
      const prompt = revokeIntroducedPrompt(device.name, introduced);
      const yes = await confirm.ask({
        title: prompt.title,
        body: prompt.body,
        items: prompt.items,
        confirmLabel: prompt.confirm,
        cancelLabel: prompt.cancel,
        tone: "danger",
      });
      if (!yes) return;
      const out = await client.revokeDevices(introduced);
      const done = revokeManyText(out, device.name);
      toast(done.text, done.tone);
    },
    [client, toast, confirm],
  );
}
