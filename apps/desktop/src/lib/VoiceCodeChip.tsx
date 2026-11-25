// The spoken code on an approval card ("say: approve 4 7"), shown only while voice is on. Large and plain so it can
// be read from across the room. Used by the pet's bubble, the Approvals view and the session's confirmation card.
import { formatVoiceCode } from "@pawl/hub-client";

export function VoiceCodeChip({ code }: { code: string | null | undefined }) {
  const digits = formatVoiceCode(code);
  if (!digits) return null;
  return (
    <span className="voice-code" role="note" aria-label={`To approve by voice, say: approve ${digits}`} title="To approve by voice, say this. Or click Approve.">
      <span className="voice-code-say">say:</span> approve <strong className="voice-code-digits">{digits}</strong>
    </span>
  );
}
