import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { AppWindow, Monitor, RefreshCw, Save, ScreenShare, X } from "lucide-react";
import { Modal } from "../../elements/Modal";
import { useAppStore } from "../../../store";
import { ScreenShareOptions } from "./ScreenShareOptions";
import { loadSharePreferences, saveSharePreferences, type CaptureSourceInfo, type EncoderReport, type NativeShareRequest, type ResolvedShare, type SharePreferences } from "./nativeSettings";
import type { ShareContext } from "./nativeBroadcast";
import styles from "./ScreenShareSetup.module.css";

interface Props {
  context: ShareContext;
  onClose: () => void;
  onStart: (request: NativeShareRequest, context: ShareContext, sourceName: string) => Promise<void>;
}

export function ScreenShareSetupDialog({ context, onClose, onStart }: Props) {
  const { t } = useTranslation("settings");
  const [value, setValue] = useState<SharePreferences | null>(null);
  const [report, setReport] = useState<EncoderReport | null>(null);
  const [sources, setSources] = useState<CaptureSourceInfo[]>([]);
  const [selectedId, setSelectedId] = useState("");
  const [kind, setKind] = useState<"monitor" | "window">("monitor");
  const [search, setSearch] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const [validation, setValidation] = useState<{ key: string; result?: ResolvedShare; error?: string } | null>(null);
  const form = useRef<HTMLFormElement>(null);
  const source = sources.find((s) => s.id === selectedId && (value?.settings.capture === "gfxcapture" || s.source.kind === "monitor"));
  const validationKey = JSON.stringify([value, source]);
  const resolved = validation?.key === validationKey ? validation.result : undefined;
  const validationError = validation?.key === validationKey ? validation.error : undefined;
  const serverId = useAppStore((s) => s.activeServerId);
  const channelId = useAppStore((s) => s.currentChannel);
  const ownSession = useAppStore((s) => s.ownSession);
  const config = useAppStore((s) => s.serverConfig);
  const contextChanged = serverId !== context.serverId || channelId !== context.channelId || ownSession !== context.ownSession;
  const transportAvailable = config.webrtc_sfu_available || (config.webrtc_p2p_relay_available && value?.settings.p2p === "auto" && value.settings.p2pMaxViewers > 0);
  const encoderAvailable = value?.settings.encoder === "auto" ? !!report?.autoSelected : !!report?.encoders.some((e) => e.id === value?.settings.encoder && e.available);

  useEffect(() => {
    const previous = document.activeElement;
    form.current?.focus();
    return () => { if (previous instanceof HTMLElement) previous.focus(); };
  }, []);
  function trapFocus(event: KeyboardEvent<HTMLFormElement>) {
    if (event.key !== "Tab") return;
    const controls = [...(form.current?.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled), select:not(:disabled)") ?? [])];
    if (!controls.length) { event.preventDefault(); return; }
    const first = controls[0], last = controls[controls.length - 1];
    if (event.shiftKey && (document.activeElement === first || document.activeElement === form.current)) { event.preventDefault(); last.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
  }

  useEffect(() => {
    let cancelled = false;
    Promise.all([loadSharePreferences(), invoke<EncoderReport>("list_video_encoders"), invoke<CaptureSourceInfo[]>("list_screen_share_sources")])
      .then(([preferences, encoders, items]) => {
        if (cancelled) return;
        setValue(preferences); setReport(encoders); setSources(items);
      }).catch((e: unknown) => { if (!cancelled) setError(String(e)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    if (!value || !source) return;
    let cancelled = false;
    const timer = setTimeout(() => {
      // Keep validation and the effective output size authoritative on the backend.
      invoke<ResolvedShare>("resolve_screen_share_encoding", {
        settings: value.settings, sourceWidth: source.size.width, sourceHeight: source.size.height,
      }).then((result) => { if (!cancelled) setValidation({ key: validationKey, result }); })
        .catch((e: unknown) => { if (!cancelled) setValidation({ key: validationKey, error: String(e) }); });
    }, 150);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [value, source, validationKey]);

  const [suggested, setSuggested] = useState<{ key: string; bitrate: number } | null>(null);
  useEffect(() => {
    if (!resolved) return;
    let cancelled = false;
    invoke<number>("suggested_screen_share_bitrate", { ...resolved.encoding.size, fps: resolved.encoding.fps })
      .then((bitrate) => { if (!cancelled) setSuggested({ key: validationKey, bitrate }); })
      .catch(() => {});
    return () => { cancelled = true; };
  }, [resolved, validationKey]);

  async function refresh() {
    setLoading(true); setError("");
    try { setSources(await invoke<CaptureSourceInfo[]>("list_screen_share_sources")); }
    catch (e) { setError(String(e)); }
    finally { setLoading(false); }
  }
  async function saveDefaults() {
    if (!value || !form.current?.reportValidity()) return;
    setBusy(true); setError("");
    try { await saveSharePreferences(value); setSaved(true); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!value || !source || !resolved || contextChanged || busy) return;
    setBusy(true); setError("");
    try { await onStart({ ...value, source: source.source }, context, source.name); onClose(); }
    catch (e) { setError(String(e)); setBusy(false); }
  }
  const activeKind = value?.settings.capture === "ddagrab" ? "monitor" : kind;
  const visible = sources.filter((s) => s.source.kind === activeKind && s.name.toLocaleLowerCase().includes(search.toLocaleLowerCase()));
  return <Modal onClose={onClose} closeOnEsc={!busy} closeOnOverlayClick={!busy}>
    <form ref={form} tabIndex={-1} onKeyDown={trapFocus} className={styles.dialog} role="dialog" aria-modal="true" aria-labelledby="screen-share-title" onSubmit={(e) => { void submit(e); }}>
      <header className={styles.header}><h2 id="screen-share-title">{t("screenShare.title")}</h2><button type="button" className={styles.iconButton} title={t("screenShare.cancel")} aria-label={t("screenShare.cancel")} disabled={busy} onClick={onClose}><X size={18} /></button></header>
      <div className={styles.body}>
        {loading && <p role="status">{t("screenShare.loading")}</p>}
        {error && <p role="alert" className={styles.error}>{error}</p>}
        {contextChanged && <p role="alert" className={styles.error}>{t("screenShare.contextChanged")}</p>}
        {!transportAvailable && value && <p role="alert" className={styles.error}>{t("screenShare.noTransport")}</p>}
        {value && report && <fieldset disabled={busy || loading} style={{ border: 0, padding: 0, margin: 0, minWidth: 0 }}>
          <div className={styles.layout}>
            <section aria-label={t("screenShare.source")}>
              <div className={styles.toolbar}>
                <div className={styles.tabs} role="tablist" aria-label={t("screenShare.source")}>
                  <button type="button" role="tab" aria-selected={activeKind === "monitor"} onClick={() => { setKind("monitor"); setSelectedId(""); }}>{t("screenShare.displays")}</button>
                  <button type="button" role="tab" aria-selected={activeKind === "window"} disabled={value.settings.capture === "ddagrab"} onClick={() => { setKind("window"); setSelectedId(""); }}>{t("screenShare.windows")}</button>
                </div>
                <button type="button" className={styles.iconButton} title={t("screenShare.refreshSources")} aria-label={t("screenShare.refreshSources")} onClick={() => { void refresh(); }}><RefreshCw size={16} /></button>
              </div>
              <input autoFocus className={styles.search} type="search" aria-label={t("screenShare.search")} placeholder={t("screenShare.search")} value={search} onChange={(e) => setSearch(e.target.value)} />
              <div className={styles.sources} role="group" aria-label={t("screenShare.source")}>
                {visible.map((item) => <label className={styles.source} key={item.id}>
                  <input type="radio" name="captureSource" checked={source?.id === item.id} onChange={() => setSelectedId(item.id)} />
                  {item.source.kind === "monitor" ? <Monitor size={24} /> : <AppWindow size={24} />}
                  <span><strong>{item.name}</strong><small>{item.size.width} x {item.size.height}</small></span>
                </label>)}
                {!visible.length && <p className={styles.status}>{t("screenShare.noSources")}</p>}
              </div>
              <p className={styles.status}>{t("screenShare.videoOnly")}</p>
            </section>
            <section aria-label={t("screenShare.encoding")}>
              <ScreenShareOptions value={value} report={report} onChange={(next) => { setValue(next); setSaved(false); }} suggestedBitrate={suggested?.key === validationKey ? suggested.bitrate : undefined} />
              {validationError && <p role="alert" className={styles.error}>{validationError}</p>}
              {resolved && <output className={styles.status}>{resolved.encoding.size.width} x {resolved.encoding.size.height} / {resolved.encoding.fps} FPS / {resolved.encoding.bitrateKbps} kbps / {resolved.encoder.id}</output>}
            </section>
          </div>
        </fieldset>}
      </div>
      <footer className={styles.footer}>
        <button type="button" className={styles.button} disabled={!value || busy || !encoderAvailable} onClick={() => { void saveDefaults(); }}><Save size={16} />{t(saved ? "screenShare.saved" : "screenShare.saveDefaults")}</button>
        <div className={styles.actions}>
          <button type="button" className={styles.button} disabled={busy} onClick={onClose}>{t("screenShare.cancel")}</button>
          <button type="submit" className={`${styles.button} ${styles.primary}`} disabled={busy || loading || !resolved || !encoderAvailable || contextChanged || !transportAvailable}><ScreenShare size={16} />{t(busy ? "screenShare.starting" : "screenShare.start")}</button>
        </div>
      </footer>
    </form>
  </Modal>;
}
