// A pending permission request inside the transcript: the AI Elements Confirmation pattern (an Item with the
// question and its answers), answered through our own flow and rules (usePermissionAnswer): another machine's
// request needs a confirm step on this device, its always-allow is chosen on that machine, nothing is sent while it
// is offline.
import { ShieldQuestionIcon } from "lucide-react";
import type { PermissionOption, PermissionRequest } from "@pawl/protocol";
import { requestCommandText, requestInputCode } from "@pawl/hub-client";
import { Button } from "@/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription, ItemFooter, ItemMedia, ItemTitle } from "@/components/ui/item";
import { agentName, permissionSummary } from "../../lib/format";
import { usePanel } from "../context";
import { usePermissionAnswer } from "../usePermissionAnswer";
import { CodeView } from "./CodeView";

const VARIANT: Record<PermissionOption["kind"], "default" | "outline" | "destructive"> = {
  allow_once: "default",
  allow_always: "outline",
  reject_once: "destructive",
  reject_always: "destructive",
};

export function ConfirmationCard({ req }: { req: PermissionRequest }) {
  const { state } = usePanel();
  const a = usePermissionAnswer(req);
  const agent = agentName(state.agents, req.agentId, req.machineId);
  // Never cut: all of what would run is shown (the block scrolls), and the confirm step repeats the command itself.
  const input = requestInputCode(req.toolCall.rawInput);
  const command = requestCommandText(req.toolCall);
  const blocked = a.busy !== null || !!a.offline || a.answered;

  const note = a.offline
    ? `${a.offline}: answer it when it is back, or on ${a.machineName}.`
    : a.problem
      ? a.problem
      : a.answered
        ? `Sent${a.remote ? ` to ${a.machineName}` : ""}…`
        : a.remote
          ? `Runs on ${a.machineName}; approving asks you to confirm first.${a.options.some((o) => a.blockedReason(o.optionId)) ? ` Choose always-allow on ${a.machineName}.` : ""}`
          : null;

  return (
    <Item variant="outline" className="border-primary/40 bg-card">
      <ItemMedia variant="icon">
        <ShieldQuestionIcon />
      </ItemMedia>
      <ItemContent>
        <ItemTitle>
          {agent} wants to: {permissionSummary(req)}
        </ItemTitle>
        {note && <ItemDescription className={a.problem ? "text-destructive" : undefined}>{note}</ItemDescription>}
      </ItemContent>
      {input && (
        <ItemFooter className="w-full">
          <div className="w-full">
            <CodeView code={input} label="What it wants to do" />
          </div>
        </ItemFooter>
      )}
      <ItemActions className="w-full flex-wrap justify-end">
        {a.confirm ? (
          <>
            <span className="w-full text-sm">
              {a.confirm.label} on <strong>{a.machineName}</strong>:
              <code className="mt-1 block max-h-40 overflow-auto rounded-md bg-muted px-2 py-1 font-mono text-xs whitespace-pre-wrap">{command}</code>
            </span>
            <Button size="sm" autoFocus disabled={a.busy !== null} onClick={() => void a.resolve(a.confirm!.optionId, a.confirm!.label, true)}>
              {a.busy === a.confirm.optionId ? "…" : `${a.confirm.label} on ${a.machineName}`}
            </Button>
            <Button size="sm" variant="ghost" disabled={a.busy !== null} onClick={() => a.cancelConfirm()}>
              Back
            </Button>
          </>
        ) : (
          <>
            {a.options.map((o) => {
              const choose = a.blockedReason(o.optionId);
              return (
                <Button
                  key={o.optionId}
                  size="sm"
                  variant={VARIANT[o.kind]}
                  disabled={blocked || !!choose}
                  title={a.offline ?? choose ?? undefined}
                  onClick={() => void a.resolve(o.optionId, o.name)}
                >
                  {a.busy === o.optionId ? "…" : a.asksConfirm(o.optionId) ? `${o.name}…` : o.name}
                </Button>
              );
            })}
            {a.needsPlainDeny && (
              <Button size="sm" variant="destructive" disabled={blocked} title={a.offline ?? undefined} onClick={() => void a.resolve(null, "Denied")}>
                Deny
              </Button>
            )}
          </>
        )}
      </ItemActions>
    </Item>
  );
}
