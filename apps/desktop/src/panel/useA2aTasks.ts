// A session's A2A tasks (list_tasks), refreshed whenever the session's status changes (a turn starts, asks, ends).
// A hub without A2A answers with an error: then there are simply no tasks.
import { useEffect, useState } from "react";
import type { A2aTaskSummary } from "@pawl/protocol";
import { usePanel } from "./context";

export function useA2aTasks(sessionId: string, status: string | undefined): A2aTaskSummary[] {
  const { client, meta } = usePanel();
  const [tasks, setTasks] = useState<A2aTaskSummary[]>([]);
  useEffect(() => {
    if (meta.phase !== "connected") return;
    let alive = true;
    client.listTasks(sessionId).then(
      (list) => alive && setTasks(list),
      () => alive && setTasks([]),
    );
    return () => {
      alive = false;
    };
  }, [client, sessionId, status, meta.phase]);
  return tasks;
}
