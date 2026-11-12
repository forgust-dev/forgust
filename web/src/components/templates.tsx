import { useEffect, useRef } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type S } from "../api/client";
import { Select } from "./controls";

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
      onChange={(name) => {
        const t = list.find((x) => x.name === name);
        const next = t?.content ?? "";
        if (description.trim() && description !== next && !list.some((x) => x.content === description) && !confirm("Replace the description with this template?")) return;
        setDescription(next);
      }}
      options={[{ value: "", label: "No template" }, ...list.map((t) => ({ value: t.name, label: t.name }))]}
    />
  );
}
