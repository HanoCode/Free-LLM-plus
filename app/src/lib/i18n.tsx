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
