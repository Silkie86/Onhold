// Tasks: everyone's to-do list, and the tasks about one enquiry or booking.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { Alert, Button, Checkbox, Dialog, type FieldIssue, Input, Label, NativeSelect, Spinner, Textarea, cn } from "@onhold/ui";
import { Plus } from "lucide-react";
import { useState } from "react";
import { api, ok } from "../api.ts";
import { type Task, fromLocalInput } from "../work.ts";
import { failureIssues } from "./crm.tsx";

const RELATED_PATH: Record<string, string> = {
  booking: "/bookings/$id",
  enquiry: "/enquiries/$id",
  contact: "/crm/contacts/$id",
  organisation: "/crm/organisations/$id",
  talent: "/crm/talent/$id",
};

function useMembers() {
  return useQuery({ queryKey: ["members"], queryFn: () => ok(api.GET("/api/v1/members")) });
}

function due(task: Task): { text: string; late: boolean } | null {
  if (!task.due_at) return null;
  const at = new Date(task.due_at);
  return {
    text: at.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }),
    late: task.status === "open" && at.getTime() < Date.now(),
  };
}

function TaskRow({ task, onChanged, showRelated }: { task: Task; onChanged: () => void; showRelated?: boolean }) {
  const [error, setError] = useState<string | null>(null);
  const when = due(task);
  const done = task.status === "done";
  const relatedPath = task.related_entity ? RELATED_PATH[task.related_entity] : undefined;
  return (
    <li className="flex items-start gap-3 px-3 py-2 text-sm">
      <Checkbox
        className="mt-0.5"
        aria-label={done ? `Reopen ${task.title}` : `Mark ${task.title} done`}
        checked={done}
        onChange={async () => {
          setError(null);
          try {
            await ok(
              api.PATCH("/api/v1/tasks/{id}", {
                params: { path: { id: task.id } },
                headers: { "If-Match": `"${task.version}"` },
                body: { status: done ? "open" : "done" },
              }),
            );
            onChanged();
          } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
          }
        }}
      />
      <div className="flex min-w-0 flex-1 flex-col">
        <span className={cn(done && "text-muted-foreground line-through")}>{task.title}</span>
        <span className="text-xs text-muted-foreground">
          {[
            when && <span key="due" className={cn(when.late && "font-medium text-destructive")}>Due {when.text}</span>,
            showRelated && task.related_id && relatedPath && (
              <Link key="rel" to={relatedPath} params={{ id: task.related_id } as never} className="underline">
                {task.related_name ?? task.related_entity}
              </Link>
            ),
            task.origin === "rule" && <span key="rule">from a rule</span>,
          ]
            .filter(Boolean)
            .flatMap((x, i) => (i ? [" · ", x] : [x]))}
        </span>
        {task.notes && <span className="whitespace-pre-line text-xs">{task.notes}</span>}
        {error && <Alert>{error}</Alert>}
      </div>
    </li>
  );
}

function NewTaskDialog({
  related,
  onClose,
  onCreated,
}: {
  related?: { entity: string; id: string };
  onClose: () => void;
  onCreated: () => void;
}) {
  const members = useMembers();
  const [title, setTitle] = useState("");
  const [notes, setNotes] = useState("");
  const [dueAt, setDueAt] = useState("");
  const [assignee, setAssignee] = useState("");
  const [issues, setIssues] = useState<FieldIssue[]>([]);
  const [error, setError] = useState<string | null>(null);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()} title="New task">
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setIssues([]);
          setError(null);
          try {
            await ok(
              api.POST("/api/v1/tasks", {
                body: {
                  title,
                  notes: notes || null,
                  due_at: fromLocalInput(dueAt),
                  assignee_id: assignee || null,
                  related_entity: related?.entity ?? null,
                  related_id: related?.id ?? null,
                },
              }),
            );
            onCreated();
            onClose();
          } catch (err) {
            const found = failureIssues(err);
            if (found) setIssues(found);
            else setError(err instanceof Error ? err.message : String(err));
          }
        }}
      >
        <Label htmlFor="task-title">What needs doing</Label>
        <Input id="task-title" required value={title} onChange={(e) => setTitle(e.target.value)} />
        <Label htmlFor="task-due">Due</Label>
        <Input id="task-due" type="datetime-local" value={dueAt} onChange={(e) => setDueAt(e.target.value)} />
        <Label htmlFor="task-assignee">For</Label>
        <NativeSelect id="task-assignee" value={assignee} onChange={(e) => setAssignee(e.target.value)}>
          <option value="">Anyone</option>
          {members.data?.map((m) => (
            <option key={m.user_id} value={m.user_id}>
              {m.name}
            </option>
          ))}
        </NativeSelect>
        <Label htmlFor="task-notes">Notes</Label>
        <Textarea id="task-notes" value={notes} onChange={(e) => setNotes(e.target.value)} />
        {issues.map((i) => (
          <Alert key={`${i.field}${i.message}`}>{i.message}</Alert>
        ))}
        {error && <Alert>{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit">Add task</Button>
        </div>
      </form>
    </Dialog>
  );
}

/** The tasks about one record, with a button to add another. */
export function RelatedTasks({ entity, id }: { entity: string; id: string }) {
  const qc = useQueryClient();
  const [adding, setAdding] = useState(false);
  const tasks = useQuery({
    queryKey: ["tasks", "related", id],
    queryFn: () => ok(api.GET("/api/v1/tasks", { params: { query: { related_id: id, status: "all", limit: 100 } } })),
  });
  const refresh = () => void qc.invalidateQueries({ queryKey: ["tasks"] });
  return (
    <section className="flex flex-col gap-3" aria-label="Tasks">
      <div className="flex items-center justify-between">
        <h2 className="font-semibold">Tasks</h2>
        <Button variant="outline" size="sm" onClick={() => setAdding(true)}>
          <Plus /> Add task
        </Button>
      </div>
      {tasks.isPending ? (
        <Spinner />
      ) : (tasks.data?.items.length ?? 0) === 0 ? (
        <p className="text-sm text-muted-foreground">Nothing to do yet.</p>
      ) : (
        <ul className="divide-y rounded-md border">
          {tasks.data?.items.map((t) => (
            <TaskRow key={t.id} task={t} onChanged={refresh} />
          ))}
        </ul>
      )}
      {adding && <NewTaskDialog related={{ entity, id }} onClose={() => setAdding(false)} onCreated={refresh} />}
    </section>
  );
}

export interface TasksSearch {
  status?: "open" | "done" | "all";
  mine?: boolean;
}

export function validateTasksSearch(s: Record<string, unknown>): TasksSearch {
  const status = s.status === "done" || s.status === "all" ? s.status : undefined;
  return { status, mine: s.mine === true || s.mine === "true" ? true : undefined };
}

export function TasksPage() {
  const search = useSearch({ from: "/app/tasks" });
  const navigate = useNavigate({ from: "/tasks" });
  const qc = useQueryClient();
  const [adding, setAdding] = useState(false);
  const status = search.status ?? "open";
  const tasks = useQuery({
    queryKey: ["tasks", "page", status, !!search.mine],
    queryFn: () =>
      ok(
        api.GET("/api/v1/tasks", {
          params: { query: { status, assignee: search.mine ? "me" : undefined, limit: 200 } },
        }),
      ),
    placeholderData: (prev) => prev,
  });
  const refresh = () => void qc.invalidateQueries({ queryKey: ["tasks"] });
  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-4 p-4 sm:p-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="flex-1 text-xl font-semibold">Tasks</h1>
        <NativeSelect
          aria-label="Show"
          className="w-auto"
          value={status}
          onChange={(e) =>
            void navigate({
              search: { ...search, status: e.target.value === "open" ? undefined : (e.target.value as "done" | "all") },
              replace: true,
            })
          }
        >
          <option value="open">To do</option>
          <option value="done">Done</option>
          <option value="all">Everything</option>
        </NativeSelect>
        <label className="flex items-center gap-2 text-sm">
          <Checkbox
            checked={!!search.mine}
            onChange={(e) => void navigate({ search: { ...search, mine: e.target.checked || undefined }, replace: true })}
          />
          Only mine
        </label>
        <Button onClick={() => setAdding(true)}>
          <Plus /> New task
        </Button>
      </div>
      {tasks.isError && <Alert>{tasks.error.message}</Alert>}
      {tasks.isPending ? (
        <Spinner />
      ) : (tasks.data?.items.length ?? 0) === 0 ? (
        <p className="py-12 text-center text-sm text-muted-foreground">
          {status === "open" ? "Nothing to do. Tasks you add, and ones your rules make, show here." : "No tasks here."}
        </p>
      ) : (
        <ul className="divide-y rounded-md border">
          {tasks.data?.items.map((t) => (
            <TaskRow key={t.id} task={t} onChanged={refresh} showRelated />
          ))}
        </ul>
      )}
      {adding && <NewTaskDialog onClose={() => setAdding(false)} onCreated={refresh} />}
    </div>
  );
}
