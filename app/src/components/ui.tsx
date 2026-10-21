import { useEffect, useState, type ReactNode } from 'react';
import { Check, Copy, X } from 'lucide-react';
import { tierLabel, useLang } from '../lib/i18n';

/* ── Badge ──────────────────────────────── */
export type Tone = 'gray' | 'accent' | 'ok' | 'warn' | 'bad' | 'info';

export function Badge({ tone = 'gray', children }: { tone?: Tone; children: ReactNode }) {
  return <span className={`badge ${tone}`}>{children}</span>;
}

/**
 * 档位徽标：文案本地化（Tier A/B/C 不直接暴露给用户）。
 * tone 可覆盖——各页对档位有各自的配色习惯，这里只统一文案，不动既有观感。
 */
export function TierBadge({ tier, tone }: { tier: string; tone?: Tone }) {
  const lang = useLang();
  const t: Tone = tone ?? (tier === 'A' ? 'ok' : tier === 'B' ? 'accent' : 'warn');
  return <Badge tone={t}>{tierLabel(tier, lang)}</Badge>;
}

export function Status({
  tone = 'ok',
  children,
}: {
  tone?: 'ok' | 'warn' | 'bad' | 'idle';
  children: ReactNode;
}) {
  return (
    <span className={`status ${tone === 'ok' ? '' : tone}`}>
      <i />
      {children}
    </span>
  );
}

/* ── 空态 ──────────────────────────────────
   全站统一的「这里真的没有数据」。刻意不接受 children 里的示例行：
   空态就该是空的，拿假数据把它填满等于骗用户自己。 */
export function Empty({
  icon,
  title,
  hint,
}: {
  icon?: ReactNode;
  title: ReactNode;
  hint?: ReactNode;
}) {
  return (
    <div className="empty">
      {icon}
      <div style={{ fontSize: 13, fontWeight: 600, color: 'var(--text)' }}>{title}</div>
      {hint && (
        <p style={{ marginTop: 6, lineHeight: 1.65, maxWidth: 520, marginInline: 'auto' }}>
          {hint}
        </p>
      )}
    </div>
  );
}

/* ── Switch ─────────────────────────────── */
export function Switch({
  on,
  onChange,
  label,
  disabled,
}: {
  on: boolean;
  onChange: (v: boolean) => void;
  label?: ReactNode;
  /**
   * 禁用态。子选项用得上：父开关关着时子开关自己切了也没效果，
   * 让它看起来可点是更糟的谎话。
   */
  disabled?: boolean;
}) {
  return (
    <button
      className={`switch ${on ? 'on' : ''}`}
      disabled={disabled}
      aria-disabled={disabled || undefined}
      onClick={() => !disabled && onChange(!on)}
    >
      <span className="tg" />
      {label}
    </button>
  );
}

/* ── Copy ───────────────────────────────── */
export function CopyBtn({ value, label }: { value: string; label?: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      className="copy-mini"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(value);
        } catch {
          /* clipboard 不可用时静默 */
        }
        setDone(true);
        setTimeout(() => setDone(false), 1200);
      }}
    >
      {done ? <Check size={11} /> : <Copy size={11} />}
      {label ?? (done ? '已复制' : '复制')}
    </button>
  );
}

/* ── Modal ────────────────────────────────
   全站唯一弹窗骨架：遮罩点击与 ESC 都关，内容自定。
   不做「点遮罩不关」的变体——需要防误关的表单先在自己的按钮上做确认。 */
export function Modal({
  title,
  subtitle,
  onClose,
  children,
  width = 440,
}: {
  title: ReactNode;
  subtitle?: ReactNode;
  onClose: () => void;
  children: ReactNode;
  width?: number;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div
      className="modal-overlay"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="modal" style={{ width }} role="dialog" aria-modal="true">
        <div className="modal-head">
          <div>
            <h3>{title}</h3>
            {subtitle && <div className="sub">{subtitle}</div>}
          </div>
          <button className="icon-btn" onClick={onClose} aria-label="close">
            <X size={13} />
          </button>
        </div>
        <div className="modal-body">{children}</div>
      </div>
    </div>
  );
}

/* ── Toast ──────────────────────────────── */
type ToastFn = (msg: string) => void;
let emit: ToastFn | null = null;

export function toast(msg: string) {
  emit?.(msg);
}

export function ToastHost() {
  const [msg, setMsg] = useState('');
  const [show, setShow] = useState(false);

  useEffect(() => {
    let timer: number;
    emit = (m: string) => {
      setMsg(m);
      setShow(true);
      clearTimeout(timer);
      timer = window.setTimeout(() => setShow(false), 2100);
    };
    return () => {
      emit = null;
      clearTimeout(timer);
    };
  }, []);

  return <div className={`toast ${show ? 'show' : ''}`}>{msg}</div>;
}

/* ── 小工具 ─────────────────────────────── */
export const fmt = {
  ctx(n: number) {
    if (n >= 1048576) return `${(n / 1048576).toFixed(0)}M`;
    if (n >= 1024) return `${Math.round(n / 1024)}K`;
    return String(n);
  },
  pct(n: number) {
    return `${Math.round(n * 100)}%`;
  },
  ms(n: number | null) {
    if (!n) return '—';
    return n >= 1000 ? `${(n / 1000).toFixed(1)}s` : `${n}ms`;
  },
};
