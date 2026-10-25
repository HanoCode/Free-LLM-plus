import { useApp } from './store';

/** 内联双语：页面里直接写 <T zh="..." en="..." />，不用维护 key 字典 */
export function T({ zh, en }: { zh: string; en: string }) {
  const lang = useApp((s) => s.lang);
  return <>{lang === 'zh' ? zh : en}</>;
}

export function useLang() {
  return useApp((s) => s.lang);
}

/** 非 JSX 场景（toast 文案、title 属性）用这个 */
export function tx(lang: 'zh' | 'en', zh: string, en: string) {
  return lang === 'zh' ? zh : en;
}

/**
 * 厂商档位的说法。界面上不出现裸的「Tier A/B/C」：
 * A = 官方免费额度，B = 匿名端点（无需 key），C = 订阅或余额付费。
 * 未知档位回落原始串，避免把没见过的值吞掉。
 */
const TIER_ZH: Record<string, string> = { A: '免费额度', B: '匿名端点', C: '订阅付费' };
const TIER_EN: Record<string, string> = { A: 'Free tier', B: 'Anonymous', C: 'Subscription' };

export function tierLabel(tier: string, lang: 'zh' | 'en') {
  const map = lang === 'zh' ? TIER_ZH : TIER_EN;
  return map[tier] ?? `Tier ${tier}`;
}

/**
 * 健康状态机的外部说法。界面上不出现裸的 `unknown` / `degraded`：
 * 未测（unknown）≠ 不可用，措辞必须让用户看出这一层。
 * 未识别的值回落原文，避免把没见过的状态吞掉。
 */
const HEALTH_ZH: Record<string, string> = {
  healthy: '可用',
  degraded: '不稳定',
  cooling: '暂停中',
  unavailable: '不可用',
  retired: '已退役',
  probing: '探测中',
  error: '异常',
  unknown: '未测',
};

const HEALTH_EN: Record<string, string> = {
  healthy: 'ready',
  degraded: 'flaky',
  cooling: 'paused',
  unavailable: 'down',
  retired: 'retired',
  probing: 'probing',
  error: 'error',
  unknown: 'untested',
};

export function healthLabel(status: string | null | undefined, lang: 'zh' | 'en') {
  const map = lang === 'zh' ? HEALTH_ZH : HEALTH_EN;
  return map[status ?? 'unknown'] ?? status ?? '—';
}
