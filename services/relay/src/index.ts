// Krypto relay Worker entry. Request checks live in gate.ts; the only way to a Durable Object is `forward`,
// which gate.ts calls after the access key, the per-IP connect budget, the pk and the Host have been checked.

import { handleRequest } from "./gate";
import { H_HOST, H_PK, type Env } from "./mailbox";

export { Mailbox } from "./mailbox";

export default {
  fetch(request: Request, env: Env): Promise<Response> {
    return handleRequest(request, {
      accessKey: env.RELAY_ACCESS_KEY,
      open: env.RELAY_OPEN,
      limiter: env.CONNECT_LIMIT,
      forward: (pk, host) => {
        // A fresh internal request: only what the mailbox needs, nothing the client sent beyond the upgrade.
        const stub = env.MAILBOX.get(env.MAILBOX.idFromName(pk));
        return stub.fetch(
          new Request("https://mailbox.internal/connect", {
            headers: { upgrade: "websocket", [H_PK]: pk, [H_HOST]: host },
          }),
        );
      },
    });
  },
} satisfies ExportedHandler<Env>;
