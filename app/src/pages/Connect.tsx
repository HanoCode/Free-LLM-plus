import { Providers } from './Providers';
import { Accounts } from './Accounts';
import { Discover } from './Discover';
import { T } from '../lib/i18n';
import { useApp, type PageId } from '../lib/store';
import { useGateway } from '../lib/gateway';

/**
 * 连接页 = 厂商 + 凭据 + 发现 的单页聚合。
 *
 * 用户心智是「把模型接进来」这一件事；厂商/凭据/发现是同一件事的三个视角，
 * 不该是三个平级导航项。三个子页保持原样复用（含各自的 page-head 与操作），
 * 这里只做 tab 切换与一行真实状态摘要。
 *
 * tab 直接复用旧 PageId（providers / accounts / discover），深链 #/providers
 * 仍然落在正确的 tab 上；侧栏只露「连接」一项。
 */

const TABS: { id: PageId; zh: string; en: string }[] = [
  { id: 'providers', zh: '厂商', en: 'Providers' },
  { id: 'accounts', zh: '凭据与额度', en: 'Credentials' },
  { id: 'discover', zh: '发现更多', en: 'Discover' },
];

const CONNECT_IDS: PageId[] = ['connect', 'providers', 'accounts', 'discover'];

export function Connect() {
  const { page, setPage } = useApp();
  const providers = useGateway((s) => s.providers);

  const tab = CONNECT_IDS.includes(page) && page !== 'connect' ? page : 'providers';
  const connected = providers.filter((p) => p.hasKey).length;
  const credentials = providers.reduce(
    (n, p) => n + p.accounts.filter((a) => a.hasKey).length,
    0,
  );

  return (
    <>
      <div className="page-head">
        <div>
          <h1>
            <T zh="连接" en="Connect" />
          </h1>
          <p>
            {connected > 0 ? (
              <T
                zh={`已接 ${connected} 个厂商 · ${credentials} 条凭据，一切就绪。`}
                en={`${connected} providers · ${credentials} credentials connected.`}
              />
            ) : (
              <T
                zh="粘一个免费 Key，或在「发现更多」里启用免 Key 的源。"
                en="Paste a free API key, or enable a keyless source under Discover."
              />
            )}
          </p>
        </div>
        <div className="head-actions connect-tabs">
          {TABS.map((t) => (
            <button
              key={t.id}
              type="button"
              className={`btn ${tab === t.id ? 'primary' : 'ghost'}`}
              onClick={() => setPage(t.id)}
            >
              <T zh={t.zh} en={t.en} />
            </button>
          ))}
        </div>
      </div>

      {tab === 'providers' && <Providers />}
      {tab === 'accounts' && <Accounts />}
      {tab === 'discover' && <Discover />}
    </>
  );
}

export { CONNECT_IDS };
