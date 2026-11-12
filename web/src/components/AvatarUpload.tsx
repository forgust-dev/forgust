import { useRef, useState, type ReactNode } from "react";
import { ImageUp, Trash2 } from "lucide-react";
import { api } from "../api/client";
import { useAct } from "../shell/toast";

/**
 * Upload or remove a picture: PNG, JPEG, GIF or WebP up to 5 MB, cropped to a
 * square on the server. `path` is the API path of the avatar (PUT / DELETE).
 */
export function AvatarUpload({ preview, path, hasOne, onChanged, what }: { preview: ReactNode; path: string; hasOne: boolean; onChanged: () => void; what: string }) {
  const input = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const act = useAct(onChanged);
  const run = async (f: () => Promise<unknown>, done: string) => {
    setBusy(true);
    await act(f, done);
    setBusy(false);
    if (input.current) input.current.value = "";
  };
  return (
    <div className="avatar-upload">
      {preview}
      <div className="stack" style={{ gap: 6 }}>
        <div className="row wrap">
          <button type="button" className="btn small" disabled={busy} onClick={() => input.current?.click()}>
            <ImageUp size={14} /> {hasOne ? "Change the picture" : "Upload a picture"}
          </button>
          {hasOne && (
            <button type="button" className="btn small ghost" disabled={busy} onClick={() => run(() => api.del(path), "Picture removed")}>
              <Trash2 size={14} /> Remove
            </button>
          )}
        </div>
        <span className="small muted">PNG, JPEG, GIF or WebP, up to 5 MB. It is cropped to a square and shown {what}.</span>
        <input
          ref={input}
          type="file"
          accept="image/png,image/jpeg,image/gif,image/webp"
          hidden
          aria-label="Picture file"
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) void run(() => api.put(path, f), "Picture updated");
          }}
        />
      </div>
    </div>
  );
}
