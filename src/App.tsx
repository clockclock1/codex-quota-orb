import { type CSSProperties, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { cursorPosition, getCurrentWindow } from "@tauri-apps/api/window";
import "./App.css";

type RateWindow = { usedPercent: number; windowDurationMins: number; resetsAt: number };
type UsageSnapshot = {
  email: string | null;
  planType: string | null;
  primary: RateWindow | null;
  secondary: RateWindow | null;
  creditBalance: string | null;
  hasCredits: boolean;
  unlimited: boolean;
  resetCredits: number;
  fetchedAt: number;
};
type FloatingSettings = {
  visible: boolean;
  pinned: boolean;
  opacity: number;
  alwaysOnTop: boolean;
  style: "card" | "orb";
  orbExpandDirection: "auto" | "left" | "right";
  displayMode: "available" | "used";
  theme: "lime" | "cyan" | "violet" | "amber" | "rose";
  proxyMode: "system" | "none" | "custom";
  proxyAddress: string;
  dataDirectory: string;
};
type OrbDragResult = { moved: boolean; atEdge: boolean; side: "left" | "right" };
type OrbPointerPress = {
  pointerId: number;
  startX: number;
  startY: number;
  cursorStart: ReturnType<typeof cursorPosition>;
};

const defaultFloatingSettings: FloatingSettings = {
  visible: false,
  pinned: false,
  opacity: 0.92,
  alwaysOnTop: true,
  style: "card",
  orbExpandDirection: "auto",
  displayMode: "available",
  theme: "lime",
  proxyMode: "system",
  proxyAddress: "",
  dataDirectory: "data",
};

const themes: { id: FloatingSettings["theme"]; label: string; color: string }[] = [
  { id: "lime", label: "青柠", color: "#c7f36b" },
  { id: "cyan", label: "海蓝", color: "#62d9e8" },
  { id: "violet", label: "紫罗兰", color: "#b69cff" },
  { id: "amber", label: "琥珀", color: "#ffc46b" },
  { id: "rose", label: "玫瑰", color: "#ff83ae" },
];

function initialFloatingSettings(): FloatingSettings {
  if ("__TAURI_INTERNALS__" in window) return defaultFloatingSettings;
  const style = new URLSearchParams(window.location.search).get("style");
  return { ...defaultFloatingSettings, style: style === "orb" ? "orb" : "card" };
}

const demoSnapshot: UsageSnapshot = {
  email: "you@example.com",
  planType: "plus",
  primary: { usedPercent: 28, windowDurationMins: 300, resetsAt: Date.now() / 1000 + 9240 },
  secondary: { usedPercent: 61, windowDurationMins: 10080, resetsAt: Date.now() / 1000 + 342000 },
  creditBalance: "0",
  hasCredits: false,
  unlimited: false,
  resetCredits: 0,
  fetchedAt: Date.now() / 1000,
};

const clamp = (value: number) => Math.min(100, Math.max(0, value));

function formatDuration(minutes: number) {
  if (minutes >= 10080) return `${Math.round(minutes / 10080)} 周`;
  if (minutes >= 1440) return `${Math.round(minutes / 1440)} 天`;
  if (minutes >= 60) return `${Math.round(minutes / 60)} 小时`;
  return `${minutes} 分钟`;
}

function formatRemaining(timestamp: number, nowMs: number) {
  const target = new Date(timestamp * 1000);
  const totalMinutes = Math.ceil((target.getTime() - nowMs) / 60_000);
  if (totalMinutes <= 0) return "即将重置";
  const days = Math.floor(totalMinutes / 1440);
  const hours = Math.floor((totalMinutes % 1440) / 60);
  const minutes = totalMinutes % 60;
  return days > 0 ? `${days} 天 ${hours} 小时` : hours > 0 ? `${hours} 小时 ${minutes} 分` : `${minutes} 分钟`;
}

function formatResetAt(timestamp: number) {
  return new Date(timestamp * 1000).toLocaleString("zh-CN", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
    timeZoneName: "short",
  });
}

function planLabel(plan: string | null) {
  if (!plan || plan === "unknown") return "ChatGPT 账户";
  return `${plan.charAt(0).toUpperCase()}${plan.slice(1)} 计划`;
}

function metricValue(usedPercent: number, mode: FloatingSettings["displayMode"]) {
  return clamp(mode === "used" ? usedPercent : 100 - usedPercent);
}

function metricLabel(mode: FloatingSettings["displayMode"]) {
  return mode === "used" ? "已使用" : "可用";
}

function oppositeDisplayMode(mode: FloatingSettings["displayMode"]): FloatingSettings["displayMode"] {
  return mode === "used" ? "available" : "used";
}

function Gauge({ usedPercent, mode, compact = false }: { usedPercent: number; mode: FloatingSettings["displayMode"]; compact?: boolean }) {
  const safeValue = metricValue(usedPercent, mode);
  const radius = compact ? 54 : 72;
  const circumference = 2 * Math.PI * radius;
  return (
    <div className={`gauge ${compact ? "gauge--compact" : ""}`} aria-label={`${metricLabel(mode)} ${Math.round(safeValue)}%`}>
      <svg viewBox="0 0 180 180" role="img">
        <circle className="gauge__track" cx="90" cy="90" r={radius} />
        <circle className="gauge__value" cx="90" cy="90" r={radius} style={{ strokeDasharray: circumference, strokeDashoffset: circumference * (1 - safeValue / 100) }} />
      </svg>
      <div className="gauge__number"><strong>{Math.round(safeValue)}</strong><span>% {metricLabel(mode)}</span></div>
    </div>
  );
}

function startNativeDrag(event: ReactMouseEvent, enabled = true) {
  if (!enabled || event.button !== 0 || !("__TAURI_INTERNALS__" in window)) return;
  const target = event.target as HTMLElement;
  if (target.closest("button, input")) return;
  void getCurrentWindow().startDragging();
}

function MainTitlebar() {
  const minimize = () => "__TAURI_INTERNALS__" in window && void getCurrentWindow().minimize();
  const maximize = () => "__TAURI_INTERNALS__" in window && void getCurrentWindow().toggleMaximize();
  const close = () => "__TAURI_INTERNALS__" in window && void getCurrentWindow().close();
  return <div className="main-titlebar" onMouseDown={(event) => startNativeDrag(event)}>
    <div className="main-titlebar__title"><i /> Codex 额度</div>
    <div className="main-titlebar__actions">
      <button onClick={minimize} aria-label="最小化"><svg viewBox="0 0 12 12"><path d="M2 8.5h8" /></svg></button>
      <button onClick={maximize} aria-label="最大化或还原"><svg viewBox="0 0 12 12"><rect x="2.5" y="2.5" width="7" height="7" /></svg></button>
      <button className="is-close" onClick={close} aria-label="关闭"><svg viewBox="0 0 12 12"><path d="m2.5 2.5 7 7m0-7-7 7" /></svg></button>
    </div>
  </div>;
}

function FloatingApp() {
  const [usage, setUsage] = useState<UsageSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  const [nowMs, setNowMs] = useState(Date.now());
  const [settings, setSettings] = useState(initialFloatingSettings);
  const [orbExpanded, setOrbExpanded] = useState(() => new URLSearchParams(window.location.search).get("expanded") === "1");
  const [orbSide, setOrbSide] = useState<"left" | "right">("right");
  const [orbAtEdge, setOrbAtEdge] = useState(true);
  const [orbReady, setOrbReady] = useState(() => !("__TAURI_INTERNALS__" in window));
  const [orbDraggingVisual, setOrbDraggingVisual] = useState(false);
  const orbDragging = useRef(false);
  const orbWasExpanded = useRef(false);
  const orbHoverTimer = useRef<number | undefined>(undefined);
  const orbTransitionToken = useRef(0);
  const orbPointerPress = useRef<OrbPointerPress | null>(null);

  useEffect(() => {
    document.documentElement.dataset.theme = settings.theme;
  }, [settings.theme]);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      setUsage("__TAURI_INTERNALS__" in window ? await invoke<UsageSnapshot>("get_codex_usage") : demoSnapshot);
      setError(false);
    } catch {
      setError(true);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if ("__TAURI_INTERNALS__" in window) {
      invoke<UsageSnapshot | null>("get_cached_usage").then((cached) => cached && setUsage(cached)).catch(() => undefined);
      invoke<FloatingSettings>("get_floating_settings").then(setSettings).catch(() => undefined);
      invoke<"left" | "right">("get_floating_orb_side").then(setOrbSide).catch(() => undefined);
    }
    const initialRefresh = window.setTimeout(refresh, 800);
    const refreshTimer = window.setInterval(refresh, 60_000);
    const clockTimer = window.setInterval(() => setNowMs(Date.now()), 1_000);
    const settingsTimer = window.setInterval(() => {
      if ("__TAURI_INTERNALS__" in window) invoke<FloatingSettings>("get_floating_settings").then(setSettings).catch(() => undefined);
    }, 2_000);
    let unlisten: (() => void) | undefined;
    if ("__TAURI_INTERNALS__" in window) {
      void listen<FloatingSettings>("floating-settings-changed", (event) => setSettings(event.payload)).then((stop) => { unlisten = stop; });
    }
    return () => {
      window.clearTimeout(initialRefresh);
      window.clearInterval(refreshTimer);
      window.clearInterval(clockTimer);
      window.clearInterval(settingsTimer);
      if (orbHoverTimer.current !== undefined) window.clearTimeout(orbHoverTimer.current);
      unlisten?.();
    };
  }, [refresh]);

  useEffect(() => {
    if (settings.style !== "orb" || !("__TAURI_INTERNALS__" in window)) return;
    invoke<"left" | "right">("get_floating_orb_side").then(setOrbSide).catch(() => undefined);
  }, [settings.style, settings.orbExpandDirection]);

  useEffect(() => {
    if (settings.style !== "orb" || !("__TAURI_INTERNALS__" in window)) return;
    let active = true;
    setOrbReady(false);
    void (async () => {
      try {
        const atEdge = await invoke<boolean>("get_floating_orb_edge_state");
        const targetExpanded = !atEdge;
        if (!active || orbDragging.current) return;
        setOrbAtEdge(atEdge);
        const side = await invoke<"left" | "right">("get_floating_orb_side");
        if (!active || orbDragging.current) return;
        setOrbSide(side);
        await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
        if (!active || orbDragging.current) return;
        const settledSide = await invoke<"left" | "right">("set_floating_orb_expanded", { expanded: targetExpanded });
        if (active) {
          setOrbSide(settledSide);
          setOrbExpanded(targetExpanded);
          setOrbReady(true);
        }
      } catch {
        if (active) setOrbReady(true);
      }
    })();
    return () => { active = false; };
  }, [settings.style]);

  const hide = () => {
    if ("__TAURI_INTERNALS__" in window) void invoke("set_floating_window", { visible: false });
  };

  const togglePin = async () => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    const pinned = await invoke<boolean>("set_floating_pinned", { pinned: !settings.pinned });
    setSettings((value) => ({ ...value, pinned }));
  };

  const setOrbOpen = async (expanded: boolean) => {
    if (settings.style !== "orb") return;
    if (orbDragging.current || orbPointerPress.current) return;
    if (orbExpanded === expanded) return;
    if (!orbAtEdge && !expanded) return;
    const token = ++orbTransitionToken.current;
    if (!("__TAURI_INTERNALS__" in window)) {
      setOrbExpanded(expanded);
      return;
    }
    try {
      if (expanded) {
        const side = await invoke<"left" | "right">("get_floating_orb_side");
        if (token !== orbTransitionToken.current) return;
        setOrbSide(side);
        await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
        if (token !== orbTransitionToken.current) return;
        const settledSide = await invoke<"left" | "right">("set_floating_orb_expanded", { expanded: true });
        if (token !== orbTransitionToken.current) return;
        setOrbSide(settledSide);
        setOrbExpanded(true);
      } else {
        setOrbExpanded(false);
        await new Promise<void>((resolve) => window.setTimeout(resolve, 210));
        if (token !== orbTransitionToken.current) return;
        const side = await invoke<"left" | "right">("set_floating_orb_expanded", { expanded: false });
        if (token === orbTransitionToken.current) setOrbSide(side);
      }
    } catch {
      if (token === orbTransitionToken.current) setOrbExpanded(!expanded);
    }
  };

  const scheduleOrbOpen = (expanded: boolean) => {
    if (orbHoverTimer.current !== undefined) window.clearTimeout(orbHoverTimer.current);
    orbHoverTimer.current = window.setTimeout(async () => {
      orbHoverTimer.current = undefined;
      if (orbPointerPress.current || orbDragging.current) return;
      if (!expanded && "__TAURI_INTERNALS__" in window) {
        try {
          if (await invoke<boolean>("get_floating_orb_pointer_inside")) return;
        } catch { /* Continue closing if the window is unavailable. */ }
      }
      void setOrbOpen(expanded);
    }, expanded ? 35 : 115);
  };

  const finishOrbDrag = useCallback((result: OrbDragResult) => {
    if (!orbDragging.current) return;
    void (async () => {
      const token = ++orbTransitionToken.current;
      const expand = !result.atEdge || (!result.moved && orbWasExpanded.current);
      setOrbAtEdge(result.atEdge);
      setOrbSide(result.side);
      await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
      if (token !== orbTransitionToken.current) return;
      const side = await invoke<"left" | "right">("set_floating_orb_expanded", { expanded: expand });
      if (token === orbTransitionToken.current) {
        setOrbSide(side);
        setOrbExpanded(expand);
      }
      orbDragging.current = false;
      setOrbDraggingVisual(false);
    })().catch(() => {
      orbDragging.current = false;
      setOrbDraggingVisual(false);
    });
  }, []);

  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let stopDragEnded: (() => void) | undefined;
    void listen<OrbDragResult>("floating-orb-drag-ended", (event) => {
      finishOrbDrag(event.payload);
    }).then((stop) => { stopDragEnded = stop; });
    return () => {
      stopDragEnded?.();
    };
  }, [finishOrbDrag]);

  const beginOrbDrag = async (press: OrbPointerPress) => {
    if (orbDragging.current || !("__TAURI_INTERNALS__" in window)) return;
    orbPointerPress.current = null;
    if (orbHoverTimer.current !== undefined) window.clearTimeout(orbHoverTimer.current);
    orbHoverTimer.current = undefined;
    ++orbTransitionToken.current;
    orbDragging.current = true;
    orbWasExpanded.current = orbExpanded;
    setOrbExpanded(false);
    setOrbDraggingVisual(true);
    try {
      const dragStart = await press.cursorStart;
      await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
      await invoke("start_floating_orb_drag", { cursorStartX: dragStart.x, cursorStartY: dragStart.y });
    } catch {
      orbDragging.current = false;
      setOrbExpanded(orbWasExpanded.current);
      setOrbDraggingVisual(false);
    }
  };

  const handleOrbPointerDown = (event: ReactPointerEvent<HTMLButtonElement>) => {
    if (event.button !== 0 || !orbReady || !("__TAURI_INTERNALS__" in window) || orbDragging.current) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    if (orbHoverTimer.current !== undefined) window.clearTimeout(orbHoverTimer.current);
    orbHoverTimer.current = undefined;
    orbPointerPress.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      cursorStart: cursorPosition(),
    };
  };

  const handleOrbPointerMove = (event: ReactPointerEvent<HTMLButtonElement>) => {
    const press = orbPointerPress.current;
    if (!press || press.pointerId !== event.pointerId) return;
    const deltaX = event.clientX - press.startX;
    const deltaY = event.clientY - press.startY;
    if (deltaX * deltaX + deltaY * deltaY < 16) return;
    void beginOrbDrag(press);
  };

  const handleOrbPointerUp = (event: ReactPointerEvent<HTMLButtonElement>) => {
    const press = orbPointerPress.current;
    if (!press || press.pointerId !== event.pointerId) return;
    orbPointerPress.current = null;
    if (orbAtEdge && !orbExpanded) scheduleOrbOpen(true);
  };

  const handleOrbPointerCancel = (event: ReactPointerEvent<HTMLButtonElement>) => {
    if (orbPointerPress.current?.pointerId === event.pointerId) orbPointerPress.current = null;
  };

  const floatingBackgroundStyle = { backgroundColor: `rgba(13, 17, 18, ${settings.opacity})` } as CSSProperties;
  const primaryMetric = metricValue(usage?.primary?.usedPercent ?? 0, settings.displayMode);
  const secondaryMetric = metricValue(usage?.secondary?.usedPercent ?? 0, settings.displayMode);

  if (settings.style === "orb") {
    return (
      <main
        className={`floating-shell floating-shell--orb is-${orbSide} ${orbExpanded ? "is-expanded" : ""} ${orbDraggingVisual ? "is-dragging" : ""}`}
        onMouseEnter={() => { if (orbReady && orbAtEdge && !orbDragging.current && !orbPointerPress.current) scheduleOrbOpen(true); }}
        onMouseLeave={() => { if (orbReady && orbAtEdge && !orbDragging.current && !orbPointerPress.current) scheduleOrbOpen(false); }}
      >
        <div className="floating-background" style={floatingBackgroundStyle} aria-hidden="true" />
        <div className={`orb-teaser ${orbExpanded ? "is-visible" : ""}`} aria-hidden={!orbExpanded} aria-label="Codex 额度摘要">
          <div className="orb-teaser__quotas">
            <div className="orb-quota-row">
              <div className="orb-quota-row__heading"><span>5 小时额度 · {metricLabel(settings.displayMode)}</span><strong>{Math.round(primaryMetric)}<small>%</small></strong></div>
              <div className="orb-teaser__track"><i style={{ width: `${primaryMetric}%` }} /></div>
              <b>{usage?.primary ? formatRemaining(usage.primary.resetsAt, nowMs) : "--"}</b>
            </div>
            <div className="orb-quota-row orb-quota-row--secondary">
              <div className="orb-quota-row__heading"><span>本周额度 · {metricLabel(settings.displayMode)}</span><strong>{Math.round(secondaryMetric)}<small>%</small></strong></div>
              <div className="orb-teaser__track"><i style={{ width: `${secondaryMetric}%` }} /></div>
              <b>{usage?.secondary ? formatRemaining(usage.secondary.resetsAt, nowMs) : "--"}</b>
            </div>
          </div>
        </div>
        <button
          className="orb-trigger"
          onPointerDown={handleOrbPointerDown}
          onPointerMove={handleOrbPointerMove}
          onPointerUp={handleOrbPointerUp}
          onPointerCancel={handleOrbPointerCancel}
          aria-label="Codex 额度悬浮球"
          title="拖动调整悬浮球位置"
        >
          <span className="orb-ring" style={{ "--orb-progress": `${primaryMetric * 3.6}deg` } as CSSProperties}><i /></span>
          <span className="orb-core"><i /><strong>{Math.round(primaryMetric)}</strong><small>%</small></span>
          <span className="orb-caption">CODEX</span>
        </button>
      </main>
    );
  }

  return (
    <main className="floating-shell">
      <div className="floating-background" style={floatingBackgroundStyle} aria-hidden="true" />
      <header className="floating-header" onMouseDown={(event) => startNativeDrag(event, !settings.pinned)}>
        <div className="floating-brand"><i /> CODEX METER</div>
        <div className="floating-actions">
          <button className={settings.pinned ? "is-active" : ""} onClick={togglePin} aria-label={settings.pinned ? "取消固定" : "固定并启用鼠标穿透"} title={settings.pinned ? "取消固定" : "固定并启用鼠标穿透"}>
            <svg viewBox="0 0 24 24"><path d="m8 4 8 8M14 3l7 7-4 1-4 4-1 4-7-7 4-1 4-4 1-4ZM5 19l4-4" /></svg>
          </button>
          <button onClick={refresh} disabled={loading} aria-label="刷新额度" title="刷新额度">
            <svg className={loading ? "is-spinning" : ""} viewBox="0 0 24 24"><path d="M20 7v5h-5M4 17v-5h5M6.1 8.1A7 7 0 0 1 18.6 7M17.9 15.9A7 7 0 0 1 5.4 17" /></svg>
          </button>
          <button onClick={hide} aria-label="隐藏悬浮窗" title="隐藏悬浮窗">
            <svg viewBox="0 0 24 24"><path d="M6 12h12" /></svg>
          </button>
        </div>
      </header>

      <section className="floating-meters">
        <div className="floating-meter">
          <span>5 小时 · {metricLabel(settings.displayMode)}</span>
          <strong>{Math.round(primaryMetric)}<small>%</small></strong>
          <div className="floating-track"><i style={{ width: `${primaryMetric}%` }} /></div>
          <b>{usage?.primary ? formatRemaining(usage.primary.resetsAt, nowMs) : "--"}</b>
        </div>
        <div className="floating-divider" />
        <div className="floating-meter">
          <span>本周 · {metricLabel(settings.displayMode)}</span>
          <strong>{Math.round(secondaryMetric)}<small>%</small></strong>
          <div className="floating-track floating-track--coral"><i style={{ width: `${secondaryMetric}%` }} /></div>
          <b>{usage?.secondary ? formatRemaining(usage.secondary.resetsAt, nowMs) : "--"}</b>
        </div>
      </section>

      <footer className="floating-footer" onMouseDown={(event) => startNativeDrag(event, !settings.pinned)}>
        <span className={error ? "is-error" : ""}><i /> {error ? "同步失败" : loading ? "同步中" : "额度可用"}</span>
        <span>{settings.pinned ? "已固定 · 鼠标穿透" : `拖动移动 · ${settings.alwaysOnTop ? "始终置顶" : "普通层级"}`}</span>
      </footer>
    </main>
  );
}

function DashboardApp() {
  const [usage, setUsage] = useState<UsageSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [demoMode, setDemoMode] = useState(false);
  const [nowMs, setNowMs] = useState(Date.now());
  const [floatingSettings, setFloatingSettings] = useState(defaultFloatingSettings);
  const [showFloatingControls, setShowFloatingControls] = useState(false);
  const [proxyAddressDraft, setProxyAddressDraft] = useState("");

  useEffect(() => {
    document.documentElement.dataset.theme = floatingSettings.theme;
  }, [floatingSettings.theme]);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      if (!("__TAURI_INTERNALS__" in window)) {
        setDemoMode(true);
        setUsage(demoSnapshot);
      } else {
        setDemoMode(false);
        setUsage(await invoke<UsageSnapshot>("get_codex_usage"));
      }
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if ("__TAURI_INTERNALS__" in window) {
      invoke<UsageSnapshot | null>("get_cached_usage").then((cached) => cached && setUsage(cached)).catch(() => undefined);
    }
    refresh();
    const refreshTimer = window.setInterval(refresh, 60_000);
    const clockTimer = window.setInterval(() => setNowMs(Date.now()), 1_000);
    return () => {
      window.clearInterval(refreshTimer);
      window.clearInterval(clockTimer);
    };
  }, [refresh]);

  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    invoke<FloatingSettings>("get_floating_settings").then((settings) => {
      setFloatingSettings(settings);
      setProxyAddressDraft(settings.proxyAddress);
    }).catch(() => undefined);
    let unlisten: (() => void) | undefined;
    void listen<FloatingSettings>("floating-settings-changed", (event) => {
      setFloatingSettings(event.payload);
    }).then((stop) => { unlisten = stop; });
    return () => unlisten?.();
  }, []);

  const toggleFloatingWindow = async () => {
    if (!("__TAURI_INTERNALS__" in window)) {
      setFloatingSettings((value) => ({ ...value, visible: !value.visible }));
      return;
    }
    try {
      const visible = await invoke<boolean>("set_floating_window", { visible: !floatingSettings.visible });
      setFloatingSettings((value) => ({ ...value, visible }));
    } catch (reason) {
      setError(String(reason));
    }
  };

  const setPinned = async () => {
    const next = !floatingSettings.pinned;
    if (!("__TAURI_INTERNALS__" in window)) {
      setFloatingSettings((value) => ({ ...value, pinned: next }));
      return;
    }
    try {
      const pinned = await invoke<boolean>("set_floating_pinned", { pinned: next });
      setFloatingSettings((value) => ({ ...value, pinned }));
    } catch (reason) { setError(String(reason)); }
  };

  const setOpacity = async (opacity: number) => {
    setFloatingSettings((value) => ({ ...value, opacity }));
    if (!("__TAURI_INTERNALS__" in window)) return;
    try {
      const saved = await invoke<number>("set_floating_opacity", { opacity });
      setFloatingSettings((value) => ({ ...value, opacity: saved }));
    } catch (reason) { setError(String(reason)); }
  };

  const setFloatingStyle = async (style: FloatingSettings["style"]) => {
    setFloatingSettings((value) => ({ ...value, style }));
    if (!("__TAURI_INTERNALS__" in window)) return;
    try {
      const settings = await invoke<FloatingSettings>("set_floating_style", { style });
      setFloatingSettings(settings);
    } catch (reason) { setError(String(reason)); }
  };

  const setOrbExpandDirection = async (direction: FloatingSettings["orbExpandDirection"]) => {
    setFloatingSettings((value) => ({ ...value, orbExpandDirection: direction }));
    if (!("__TAURI_INTERNALS__" in window)) return;
    try {
      const settings = await invoke<FloatingSettings>("set_floating_orb_expand_direction", { direction });
      setFloatingSettings(settings);
    } catch (reason) { setError(String(reason)); }
  };

  const setDisplayMode = async (mode: FloatingSettings["displayMode"]) => {
    setFloatingSettings((value) => ({ ...value, displayMode: mode }));
    if (!("__TAURI_INTERNALS__" in window)) return;
    try {
      const saved = await invoke<FloatingSettings["displayMode"]>("set_display_mode", { mode });
      setFloatingSettings((value) => ({ ...value, displayMode: saved }));
    } catch (reason) { setError(String(reason)); }
  };

  const setTheme = async (theme: FloatingSettings["theme"]) => {
    setFloatingSettings((value) => ({ ...value, theme }));
    if (!("__TAURI_INTERNALS__" in window)) return;
    try {
      const saved = await invoke<FloatingSettings["theme"]>("set_theme", { theme });
      setFloatingSettings((value) => ({ ...value, theme: saved }));
    } catch (reason) { setError(String(reason)); }
  };

  const saveProxy = async (mode: FloatingSettings["proxyMode"], address = proxyAddressDraft) => {
    if (!("__TAURI_INTERNALS__" in window)) {
      setFloatingSettings((value) => ({ ...value, proxyMode: mode, proxyAddress: address }));
      return;
    }
    try {
      const settings = await invoke<FloatingSettings>("set_proxy_settings", { mode, address });
      setFloatingSettings(settings);
      setProxyAddressDraft(settings.proxyAddress);
    } catch (reason) { setError(String(reason)); }
  };

  const updatedAt = useMemo(() => usage ? new Date(usage.fetchedAt * 1000).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false }) : "尚未同步", [usage]);
  const primary = usage?.primary;
  const secondary = usage?.secondary;
  const primaryOppositeMode = oppositeDisplayMode(floatingSettings.displayMode);
  const primaryOppositeMetric = metricValue(primary?.usedPercent ?? 0, primaryOppositeMode);
  const secondaryMetric = metricValue(secondary?.usedPercent ?? 0, floatingSettings.displayMode);

  return (
    <main className="app-shell">
      <MainTitlebar />
      <div className="ambient ambient--one" /><div className="ambient ambient--two" />
      <header className="topbar">
        <div className="brand"><div className="brand__mark"><span /></div><div><div className="brand__name">CODEX METER</div><div className="brand__sub">本机账户额度监测</div></div></div>
        <div className="topbar__actions">
          {demoMode && <span className="demo-pill">浏览器演示</span>}
          <div className="sync-state"><i className={error ? "is-error" : ""} /><span>{error ? "同步失败" : `更新于 ${updatedAt}`}</span></div>
          <button className={`floating-toggle ${floatingSettings.visible ? "is-active" : ""}`} onClick={toggleFloatingWindow} aria-pressed={floatingSettings.visible} aria-label={floatingSettings.visible ? "关闭悬浮窗" : "开启悬浮窗"}>
            <span className="toggle-track"><i /></span>
            悬浮窗
          </button>
          <div className="floating-settings-wrap">
            <button className={`settings-button ${showFloatingControls ? "is-active" : ""}`} onClick={() => setShowFloatingControls((value) => !value)} aria-expanded={showFloatingControls} aria-label="应用设置" title="应用设置">
              <svg viewBox="0 0 24 24"><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7ZM19.4 15a1.7 1.7 0 0 0 .34 1.88l.06.06-2.83 2.83-.06-.06A1.7 1.7 0 0 0 15 19.4a1.7 1.7 0 0 0-1 .6V20h-4v-.08a1.7 1.7 0 0 0-1-.6 1.7 1.7 0 0 0-1.88.34l-.06.06-2.83-2.83.06-.06A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-.6-1H4v-4h.08a1.7 1.7 0 0 0 .6-1 1.7 1.7 0 0 0-.34-1.88l-.06-.06 2.83-2.83.06.06A1.7 1.7 0 0 0 9 4.6a1.7 1.7 0 0 0 1-.6V4h4v.08a1.7 1.7 0 0 0 1 .6 1.7 1.7 0 0 0 1.88-.34l.06-.06 2.83 2.83-.06.06A1.7 1.7 0 0 0 19.4 9c.12.38.33.72.6 1h.08v4H20c-.27.28-.48.62-.6 1Z" /></svg>
            </button>
            {showFloatingControls && <section className="floating-popover">
              <div className="popover-heading"><div><span>显示与连接</span><strong>CODEX METER / SETTINGS</strong></div><span className="settings-state">自动保存</span></div>
              <div className="settings-block">
                <span className="settings-label">额度显示形式</span>
                <div className="segment-control">
                  <button className={floatingSettings.displayMode === "available" ? "is-active" : ""} onClick={() => void setDisplayMode("available")}>显示可用</button>
                  <button className={floatingSettings.displayMode === "used" ? "is-active" : ""} onClick={() => void setDisplayMode("used")}>显示已使用</button>
                </div>
              </div>
              <div className="settings-block">
                <span className="settings-label">主题配色</span>
                <div className="theme-options" role="group" aria-label="主题配色">
                  {themes.map((theme) => <button key={theme.id} className={`theme-option ${floatingSettings.theme === theme.id ? "is-active" : ""}`} onClick={() => void setTheme(theme.id)} aria-pressed={floatingSettings.theme === theme.id} aria-label={`${theme.label}主题`} title={`${theme.label}主题`}>
                    <i style={{ backgroundColor: theme.color }} />{theme.label}
                  </button>)}
                </div>
              </div>
              <div className="settings-block">
                <span className="settings-label">悬浮窗样式</span>
                <div className="segment-control">
                  <button className={floatingSettings.style === "card" ? "is-active" : ""} onClick={() => void setFloatingStyle("card")}>紧凑卡片</button>
                  <button className={floatingSettings.style === "orb" ? "is-active" : ""} onClick={() => void setFloatingStyle("orb")}>浮动小球</button>
                </div>
                <small className="settings-hint">贴边后收起为独立圆球，悬停展开额度摘要；不贴边时保持展开，拖动靠近屏幕边缘会自动吸附。</small>
              </div>
              {floatingSettings.style === "orb" && <div className="settings-block">
                <span className="settings-label">小球展开方向</span>
                <div className="segment-control segment-control--three">
                  <button className={floatingSettings.orbExpandDirection === "auto" ? "is-active" : ""} onClick={() => void setOrbExpandDirection("auto")}>自动</button>
                  <button className={floatingSettings.orbExpandDirection === "left" ? "is-active" : ""} onClick={() => void setOrbExpandDirection("left")}>向左展开</button>
                  <button className={floatingSettings.orbExpandDirection === "right" ? "is-active" : ""} onClick={() => void setOrbExpandDirection("right")}>向右展开</button>
                </div>
                <small className="settings-hint">自动按小球所在屏幕一侧展开；也可以固定向左或向右展开。</small>
              </div>}
              <div className="settings-block settings-block--inline">
                <div><span className="settings-label">悬浮窗固定</span><small>{floatingSettings.pinned ? "主体鼠标穿透" : "可自由拖动"}</small></div>
                <button className={`compact-action ${floatingSettings.pinned ? "is-active" : ""}`} onClick={setPinned} disabled={floatingSettings.style === "orb"}>{floatingSettings.style === "orb" ? "小球可交互" : floatingSettings.pinned ? "取消固定" : "固定"}</button>
              </div>
              <div className="settings-block settings-block--inline">
                <div><span className="settings-label">窗口置顶</span><small>持续置顶，高于普通窗口层级</small></div>
                <span className="topmost-badge">始终</span>
              </div>
              <label className="opacity-control"><span>黑色背景透明度 <b>{Math.round((1 - floatingSettings.opacity) * 100)}%</b></span><input type="range" min="0" max="100" value={Math.round((1 - floatingSettings.opacity) * 100)} onChange={(event) => void setOpacity(1 - Number(event.target.value) / 100)} /></label>
              <div className="settings-block proxy-settings">
                <span className="settings-label">网络代理</span>
                <div className="proxy-options">
                  <label><input type="radio" name="proxy" checked={floatingSettings.proxyMode === "system"} onChange={() => void saveProxy("system")} />系统代理</label>
                  <label><input type="radio" name="proxy" checked={floatingSettings.proxyMode === "none"} onChange={() => void saveProxy("none")} />无代理</label>
                  <label><input type="radio" name="proxy" checked={floatingSettings.proxyMode === "custom"} onChange={() => setFloatingSettings((value) => ({ ...value, proxyMode: "custom" }))} />本地代理</label>
                </div>
                {floatingSettings.proxyMode === "custom" && <div className="proxy-input-row"><input value={proxyAddressDraft} onChange={(event) => setProxyAddressDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter") void saveProxy("custom"); }} placeholder="http://127.0.0.1:7890" aria-label="本地代理地址" /><button onClick={() => void saveProxy("custom")}>保存</button></div>}
              </div>
              <p>{floatingSettings.pinned ? "固定后主体会穿透鼠标，顶部图钉仍可取消固定。" : "设置会写入 EXE 同目录的 data 文件夹。"}</p>
              <small title={floatingSettings.dataDirectory}>设置自动保存在 data 文件夹</small>
            </section>}
          </div>
          <button className="refresh-button" onClick={refresh} disabled={loading} aria-label="刷新额度">
            <svg className={loading ? "is-spinning" : ""} viewBox="0 0 24 24" aria-hidden="true"><path d="M20 7v5h-5M4 17v-5h5M6.1 8.1A7 7 0 0 1 18.6 7M17.9 15.9A7 7 0 0 1 5.4 17" /></svg>
            <span className="refresh-label">{loading ? "同步中" : "刷 新"}</span>
          </button>
        </div>
      </header>

      {error && !usage ? (
        <section className="error-panel"><span className="error-panel__code">CONNECTION / 01</span><h1>还没读到额度</h1><p>{error}</p><button onClick={refresh}>重新连接</button></section>
      ) : (
        <>
          <section className="hero-grid">
            <article className="quota-card quota-card--primary">
              <div className="card-heading"><div><span className="eyebrow">PRIMARY WINDOW</span><h1>{primary ? formatDuration(primary.windowDurationMins) : "短周期"}额度</h1></div><span className="live-badge"><i /> LIVE</span></div>
              <div className="primary-layout">
                <Gauge usedPercent={primary?.usedPercent ?? 0} mode={floatingSettings.displayMode} />
                <div className="quota-copy"><p>本周期{metricLabel(primaryOppositeMode)}</p><strong>{Math.round(primaryOppositeMetric)}<small>%</small></strong><div className="divider" /><p>服务端重置时间</p><b>{primary ? formatResetAt(primary.resetsAt) : "等待数据"}</b>{primary && <span className="reset-countdown">剩余 {formatRemaining(primary.resetsAt, nowMs)}</span>}</div>
              </div>
              <div className="card-note">额度来自本机 Codex 会话 · 每分钟自动同步</div>
            </article>

            <article className="quota-card quota-card--secondary">
              <div className="card-heading"><div><span className="eyebrow">SECONDARY WINDOW</span><h2>{secondary ? formatDuration(secondary.windowDurationMins) : "长期"}额度</h2></div><span className="index-label">02</span></div>
              <Gauge usedPercent={secondary?.usedPercent ?? 0} mode={floatingSettings.displayMode} compact />
              <div className="bar-copy"><span>{metricLabel(floatingSettings.displayMode)} {Math.round(secondaryMetric)}%</span><span>{floatingSettings.displayMode === "used" ? "可用" : "已使用"} {Math.round(100 - secondaryMetric)}%</span></div>
              <div className="progress-track"><span style={{ width: `${secondaryMetric}%` }} /></div>
              <div className="reset-row"><span>服务端重置时间</span><div><strong>{secondary ? formatResetAt(secondary.resetsAt) : "等待数据"}</strong>{secondary && <small>剩余 {formatRemaining(secondary.resetsAt, nowMs)}</small>}</div></div>
            </article>
          </section>

          <section className="account-strip">
            <div className="account-identity"><span className="avatar">{usage?.email?.slice(0, 1).toUpperCase() || "C"}</span><div><span>当前账户</span><strong>{usage?.email || "已连接 Codex"}</strong></div></div>
            <div className="stat"><span>订阅</span><strong>{planLabel(usage?.planType ?? null)}</strong></div>
            <div className="stat"><span>可用余额</span><strong>{usage?.unlimited ? "无限" : usage?.hasCredits ? usage.creditBalance : "未启用"}</strong></div>
            <div className="stat"><span>重置券</span><strong>{usage?.resetCredits ?? 0} 张</strong></div>
          </section>
        </>
      )}
      <footer><span>CODEX METER / WINDOWS</span><span>LOCAL DATA · PRIVATE BY DESIGN</span></footer>
    </main>
  );
}

function App() {
  const isFloating = new URLSearchParams(window.location.search).get("view") === "floating";
  if (isFloating) document.documentElement.classList.add("floating-document");
  return isFloating ? <FloatingApp /> : <DashboardApp />;
}

export default App;
