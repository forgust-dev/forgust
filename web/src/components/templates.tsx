import { useEffect, useRef } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type S } from "../api/client";
import { Select } from "./controls";
import { useConfirm } from "../shell/dialog";

type Kind = "merge_request" | "issue";

export function useTemplates(projectId: number, kind: Kind) {
  return useQuery({
    queryKey: ["templates", projectId, kind],
    queryFn: () => api.get<S["DescriptionTemplate"][]>(`/projects/${projectId}/templates/${kind}`),
    staleTime: 60_000,
  });
}

/**
 * Description templates from the repository: "Default" fills an empty
 * description by itself; the others are picked from a list.
 */
export function TemplatePicker({ projectId, kind, description, setDescription }: { projectId: number; kind: Kind; description: string; setDescription: (d: string) => void }) {
  const q = useTemplates(projectId, kind);
  const confirm = useConfirm();
  const list = q.data ?? [];
  const filled = useRef(false);
  useEffect(() => {
    if (filled.current || !q.data) return;
    filled.current = true;
    const def = q.data.find((t) => t.name.toLowerCase() === "default");
    if (def && !description.trim()) setDescription(def.content);
  }, [q.data]);
  if (list.length === 0) return null;
  const current = list.find((t) => t.content === description)?.name ?? "";
  return (
    <Select
      label="Description template"
      value={current}
      onChange={async (name) => {
        const t = list.find((x) => x.name === name);
        const next = t?.content ?? "";
        // what you wrote yourself (not another template) is asked about first
        const yours = description.trim() && description !== next && !list.some((x) => x.content === description);
        if (yours && !(await confirm({ title: "Replace the description?", body: "What you wrote is replaced by this template.", action: "Replace", danger: true }))) return;
        setDescription(next);
      }}
      options={[{ value: "", label: "No template" }, ...list.map((t) => ({ value: t.name, label: t.name }))]}
    />
  );
}
