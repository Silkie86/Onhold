// The people in an act: who they are, their role, and notes for travel and
// hospitality.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { Schemas } from "@onhold/api-client";
import { Alert, Button, Input, Label, Spinner, Textarea } from "@onhold/ui";
import { Pencil, Plus, Trash2 } from "lucide-react";
import { type FormEvent, useState } from "react";
import { api, ok } from "../api.ts";

type Member = Schemas["TalentMemberView"];
type MemberInput = Schemas["TalentMemberInput"];

export function TalentMembers({ talentId, canWrite }: { talentId: string; canWrite: boolean }) {
  const qc = useQueryClient();
  const key = ["talent-members", talentId];
  const members = useQuery({
    queryKey: key,
    queryFn: () => ok(api.GET("/api/v1/talent/{id}/members", { params: { path: { id: talentId } } })),
  });
  const [editing, setEditing] = useState<Member | "new" | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = () => {
    void qc.invalidateQueries({ queryKey: key });
    void qc.invalidateQueries({ queryKey: ["timeline", "talent", talentId] });
  };
  const save = async (input: MemberInput) => {
    if (editing === "new") {
      await ok(api.POST("/api/v1/talent/{id}/members", { params: { path: { id: talentId } }, body: input }));
    } else if (editing) {
      await ok(
        api.PUT("/api/v1/talent/{id}/members/{member_id}", {
          params: { path: { id: talentId, member_id: editing.id } },
          body: input,
        }),
      );
    }
    setEditing(null);
    refresh();
  };
  const remove = async (m: Member) => {
    if (!window.confirm(`Remove ${m.name}?`)) return;
    try {
      await ok(api.DELETE("/api/v1/talent/{id}/members/{member_id}", { params: { path: { id: talentId, member_id: m.id } } }));
      refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <section className="flex flex-col gap-3" aria-label="Members">
      <div className="flex items-center justify-between">
        <h2 className="font-semibold">Members</h2>
        {canWrite && editing === null && (
          <Button variant="outline" size="sm" onClick={() => setEditing("new")}>
            <Plus /> Add member
          </Button>
        )}
      </div>
      {error && <Alert>{error}</Alert>}
      {members.isPending && <Spinner />}
      {members.data && members.data.length === 0 && editing === null && (
        <p className="text-sm text-muted-foreground">No members. Add the people in an act or crew.</p>
      )}
      <ul className="flex flex-col gap-2">
        {members.data?.map((m) =>
          editing !== "new" && editing?.id === m.id ? (
            <li key={m.id}>
              <MemberForm member={m} onSave={save} onCancel={() => setEditing(null)} />
            </li>
          ) : (
            <li key={m.id} className="flex items-start justify-between gap-2 rounded-md border px-3 py-2 text-sm">
              <div className="min-w-0">
                <p className="font-medium">
                  {m.name}
                  {m.role && <span className="font-normal text-muted-foreground"> · {m.role}</span>}
                </p>
                <p className="text-muted-foreground">{[m.email, m.phone].filter(Boolean).join(" · ")}</p>
                {m.notes && <p className="mt-1 whitespace-pre-line">{m.notes}</p>}
              </div>
              {canWrite && (
                <div className="flex shrink-0 gap-1">
                  <Button variant="ghost" size="icon" aria-label={`Edit ${m.name}`} onClick={() => setEditing(m)}>
                    <Pencil />
                  </Button>
                  <Button variant="ghost" size="icon" aria-label={`Remove ${m.name}`} onClick={() => void remove(m)}>
                    <Trash2 />
                  </Button>
                </div>
              )}
            </li>
          ),
        )}
      </ul>
      {editing === "new" && <MemberForm onSave={save} onCancel={() => setEditing(null)} />}
    </section>
  );
}

function MemberForm({
  member,
  onSave,
  onCancel,
}: {
  member?: Member;
  onSave: (m: MemberInput) => Promise<void>;
  onCancel: () => void;
}) {
  const [v, setV] = useState({
    name: member?.name ?? "",
    role: member?.role ?? "",
    email: member?.email ?? "",
    phone: member?.phone ?? "",
    notes: member?.notes ?? "",
  });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const field = (k: keyof typeof v, label: string, type = "text") => (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={`member-${k}`}>{label}</Label>
      <Input id={`member-${k}`} type={type} value={v[k]} onChange={(e) => setV({ ...v, [k]: e.target.value })} />
    </div>
  );
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const blank = (s: string) => s.trim() || null;
      await onSave({
        name: v.name,
        role: blank(v.role),
        email: blank(v.email),
        phone: blank(v.phone),
        notes: blank(v.notes),
        position: member?.position,
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className="grid gap-3 rounded-md border p-3 sm:grid-cols-2" onSubmit={submit}>
      {field("name", "Name")}
      {field("role", "Role")}
      {field("email", "Email", "email")}
      {field("phone", "Phone", "tel")}
      <div className="flex flex-col gap-1.5 sm:col-span-2">
        <Label htmlFor="member-notes">Notes (dietary, travel…)</Label>
        <Textarea id="member-notes" value={v.notes} onChange={(e) => setV({ ...v, notes: e.target.value })} />
      </div>
      {error && <Alert className="sm:col-span-2">{error}</Alert>}
      <div className="flex justify-end gap-2 sm:col-span-2">
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" disabled={busy || !v.name.trim()}>
          {busy && <Spinner />}
          {member ? "Save" : "Add"}
        </Button>
      </div>
    </form>
  );
}
