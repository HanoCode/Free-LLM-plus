import { useEffect, useState, type ReactNode } from 'react';
import { Check, Copy } from 'lucide-react';

/* ── Badge ──────────────────────────────── */
export type Tone = 'gray' | 'accent' | 'ok' | 'warn' | 'bad' | 'info';

export function Badge({ tone = 'gray', children }: { tone?: Tone; children: ReactNode }) {
  return <span className={`badge ${tone}`}>{children}</span>;
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
}: {
  on: boolean;
  onChange: (v: boolean) => void;
  label?: ReactNode;
}) {
  return (
    <button className={`switch ${on ? 'on' : ''}`} onClick={() => onChange(!on)}>
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
