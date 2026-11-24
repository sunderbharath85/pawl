// The only logging the relay does: an event name plus numeric counts. The signature makes it impossible to log
// envelopes, keys, tokens or push tokens by accident.

export function log(event: string, counts?: Record<string, number>): void {
  console.log(JSON.stringify(counts ? { relay: event, ...counts } : { relay: event }));
}
