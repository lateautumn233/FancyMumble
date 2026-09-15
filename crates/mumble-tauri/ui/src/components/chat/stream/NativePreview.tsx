import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { RefreshCw } from "lucide-react";
import { fetchNativePreviewFrame } from "./nativePreviewConnection";
import type { NativeStatus } from "./nativeBroadcast";
import styles from "./ScreenShareViewer.module.css";

export function NativePreview({ status }: { readonly status: NativeStatus }) {
  const { t } = useTranslation(["chat", "common"]);
  const imageRef = useRef<HTMLImageElement>(null);
  const [error, setError] = useState("");
  const [ready, setReady] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    setError("");
    setReady(false);
    let disposed = false;
    let inFlight = false;
    let lastSequence = 0;
    const refresh = async () => {
      if (disposed || inFlight) return;
      inFlight = true;
      try {
        const frame = await fetchNativePreviewFrame(status);
        if (disposed || !frame || frame.sequence === lastSequence) return;
        lastSequence = frame.sequence;
        if (imageRef.current) imageRef.current.src = `data:${frame.mime};base64,${frame.data}`;
        setReady(true);
        setError("");
      } catch (e) {
        if (!disposed) setError(String(e));
      } finally {
        inFlight = false;
      }
    };
    void refresh();
    // Poll faster than the capture frame interval; the backend cache makes
    // repeated requests cheap when the desktop is idle.
    const frameRate = Math.max(1, status.fps ?? 30);
    const timer = setInterval(() => { void refresh(); }, Math.max(1, Math.round(1000 / frameRate)));
    return () => { disposed = true; clearInterval(timer); };
  }, [status.serverId, status.broadcastId, status.fps, attempt]);
  return <div className={styles.streamViewport}>
    <img ref={imageRef} className={styles.videoElement} onLoad={() => setReady(true)}
      alt={t("screenShare.preview")} />
    {(!ready || error) && <div className={styles.previewStatus} role={error ? "alert" : "status"}>
      {error || t("screenShare.connecting")}
      {error && <button className={styles.controlBtn} onClick={() => setAttempt((n) => n + 1)}
        title={t("common:actions.retry")} aria-label={t("common:actions.retry")}><RefreshCw size={16} /></button>}
    </div>}
  </div>;
}
