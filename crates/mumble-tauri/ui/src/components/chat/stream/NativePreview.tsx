import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { RefreshCw } from "lucide-react";
import { connectNativePreview } from "./nativePreviewConnection";
import type { NativeStatus } from "./nativeBroadcast";
import styles from "./ScreenShareViewer.module.css";

export function NativePreview({ status }: { readonly status: NativeStatus }) {
  const { t } = useTranslation(["chat", "common"]);
  const videoRef = useRef<HTMLVideoElement>(null);
  const [error, setError] = useState("");
  const [ready, setReady] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    setError("");
    setReady(false);
    let dispose: (() => void) | undefined;
    try {
      dispose = connectNativePreview(status, (stream) => {
        video.srcObject = stream;
        void video.play().catch((e: unknown) => setError(String(e)));
      }, setError);
    } catch (e) { setError(String(e)); }
    return () => { dispose?.(); video.srcObject = null; };
  }, [status.serverId, status.broadcastId, attempt]);
  return <div className={styles.streamViewport}>
    <video ref={videoRef} autoPlay playsInline muted className={styles.videoElement}
      onPlaying={() => setReady(true)} aria-label={t("screenShare.preview")} />
    {(!ready || error) && <div className={styles.previewStatus} role={error ? "alert" : "status"}>
      {error || t("screenShare.connecting")}
      {error && <button className={styles.controlBtn} onClick={() => setAttempt((n) => n + 1)}
        title={t("common:actions.retry")} aria-label={t("common:actions.retry")}><RefreshCw size={16} /></button>}
    </div>}
  </div>;
}
