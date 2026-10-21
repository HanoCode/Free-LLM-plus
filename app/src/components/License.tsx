import { useEffect, useState } from 'react';
import { Badge, CopyBtn, Modal, toast, type Tone } from './ui';
import { T } from '../lib/i18n';
import { useGateway } from '../lib/gateway';
import { useApp } from '../lib/store';

/**
 * 许可证（Pro）。
 *
 * 这一栏的措辞比别处更要小心：它既不能说得像「解锁按钮」，也不能让
 * 免费用户觉得自己在用残废版。事实是——转发、路由、巡检、注入全部
 * 免费且完整，Pro 卖的是**需要服务端持续供给的数据**（实时目录、
 * 额度情报、云端同步）。所以这里必须把「过期不影响转发」写在界面上，
 * 而不是只写在文档里。
 *
 * 激活动作收进全局弹窗（LicenseDialog）：侧边栏、设置页都能叫开，
 * 激活码粘贴、校验、报错都在弹窗里完成；卡片只负责展示状态与授权详情。
 */

const statusMeta: Record<string, { tone: Tone; zh: string; en: string }> = {
  missing: { tone: 'gray', zh: '免费版', en: 'Free' },
  valid: { tone: 'ok', zh: 'Pro 有效', en: 'Pro active' },
  grace: { tone: 'warn', zh: '宽限期', en: 'Grace period' },
  expired: { tone: 'bad', zh: '已过期', en: 'Expired' },
  invalid: { tone: 'bad', zh: '无效', en: 'Invalid' },
  wrong_machine: { tone: 'bad', zh: '机器不符', en: 'Wrong machine' },
};

function day(ts?: number): string {
  if (!ts) return '—';
  const d = new Date(ts * 1000);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(
    d.getDate(),
  ).padStart(2, '0')}`;
}

/** 授权详情行。激活与「买过但过期」两种状态下都要能看清楚买到了什么。 */
function DetailRows() {
  const license = useGateway((s) => s.license);
  return (
    <>
      <div className="set-row">
        <div>
          <div className="k">
            <T zh="授权给" en="Licensed to" />
          </div>
          <div className="d">{license?.subject ?? '—'}</div>
        </div>
        <div className="v mono">{license?.seats ? `${license.seats} seats` : ''}</div>
      </div>
      <div className="set-row">
        <div>
          <div className="k">
            <T zh="到期" en="Expires" />
          </div>
          <div className="d">
            {day(license?.expiresAt)}
            {typeof license?.daysLeft === 'number' && (
              <>
                {' · '}
                {license.daysLeft >= 0 ? (
                  <T
                    zh={`还剩 ${license.daysLeft} 天`}
                    en={`${license.daysLeft} days left`}
                  />
                ) : (
                  <T
                    zh={`已过期 ${-license.daysLeft} 天`}
                    en={`expired ${-license.daysLeft} days ago`}
                  />
                )}
              </>
            )}
            {license?.status === 'grace' && license.graceUntil && (
              <>
                {' · '}
                <T
                  zh={`${day(license.graceUntil)} 前宽限`}
                  en={`grace until ${day(license.graceUntil)}`}
                />
              </>
            )}
          </div>
        </div>
      </div>
      {license?.features && license.features.length > 0 && (
        <div className="set-row">
          <div>
            <div className="k">
              <T zh="包含能力" en="Included" />
            </div>
            <div className="d" style={{ display: 'flex', flexWrap: 'wrap', gap: 4 }}>
              {license.features.map((f) => (
                <span key={f} className="badge accent" style={{ fontSize: 10 }}>
                  {f}
                </span>
              ))}
            </div>
          </div>
        </div>
      )}
      <div className="set-row">
        <div>
          <div className="k">
            <T zh="本机安装码" en="Machine ID" />
          </div>
          <div className="d mono">{license?.machineId || '—'}</div>
        </div>
        <div className="v">{license?.machineId && <CopyBtn value={license.machineId} />}</div>
      </div>
      {license?.message && (
        <div className={`license-note ${license.status === 'grace' ? 'warn' : 'bad'}`}>
          {license.message}
        </div>
      )}
    </>
  );
}

export function License() {
  const license = useGateway((s) => s.license);
  const clear = useGateway((s) => s.clearLicense);
  const openDialog = useApp((s) => s.setLicenseDialog);

  const meta = statusMeta[license?.status ?? 'missing'] ?? statusMeta.missing;
  const active = !!license?.pro;

  return (
    <div className="card">
      <div className="card-head">
        <h3 style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <T zh="许可证" en="License" />
          <Badge tone={meta.tone}>
            <T zh={meta.zh} en={meta.en} />
          </Badge>
        </h3>
        {license?.licensedTier === 'pro' && license?.status !== 'valid' && (
          <span className="sub">
            <T zh="买过 Pro" en="Pro purchased" />
          </span>
        )}
      </div>

      {active ? (
        <>
          <DetailRows />
          <div style={{ display: 'flex', gap: 8, padding: '0 16px 14px' }}>
            <button className="btn ghost sm" onClick={() => openDialog(true)}>
              <T zh="更换许可证" en="Change license" />
            </button>
            <button
              className="btn ghost sm"
              onClick={() => {
                void clear().then(() => toast('已解除激活，回到免费版'));
              }}
            >
              <T zh="解除激活" en="Deactivate" />
            </button>
          </div>
        </>
      ) : (
        <>
          <div className="set-row">
            <div>
              <div className="k">
                <T zh="激活码" en="License key" />
              </div>
              <div className="d">
                <T
                  zh="粘贴邮件里收到的许可证。离线验签，不联网、不上传。"
                  en="Paste the license from your email. Verified offline — never uploaded."
                />
              </div>
            </div>
          </div>
          <div style={{ padding: '0 16px 14px' }}>
            {license?.message && (
              <div
                className={`license-note ${license.status === 'expired' ? 'warn' : 'bad'}`}
                style={{ margin: '0 0 8px' }}
              >
                {license.message}
              </div>
            )}
            <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <button className="btn sm" disabled={!license} onClick={() => openDialog(true)}>
                <T zh={license?.licensedTier === 'pro' ? '重新输入激活码' : '输入激活码'} en={license?.licensedTier === 'pro' ? 'Enter a new key' : 'Enter license key'} />
              </button>
              <span className="sub" style={{ marginLeft: 'auto' }}>
                <T zh="本机安装码" en="Machine ID" />{' '}
                <span className="mono faint">{license?.machineId ?? '—'}</span>
              </span>
              {license?.machineId && <CopyBtn value={license.machineId} />}
            </div>
          </div>
        </>
      )}

      <div className="license-foot">
        <T
          zh="许可证只决定 Pro 数据服务（实时目录、额度情报、多机同步）。转发、路由、巡检与注入对所有人完整可用——过期不会中断任何请求。"
          en="The license only gates Pro data services (live catalog, quota intel, sync). Forwarding, routing, patrol and injection stay fully available to everyone — expiry never interrupts a request."
        />
      </div>
    </div>
  );
}

/**
 * 全局激活弹窗。挂在 App 根部：侧边栏的「输入激活码」、设置页的
 * 「输入/更换许可证」打开的都是这一个实例，状态不会互相踩。
 */
export function LicenseDialog() {
  const open = useApp((s) => s.licenseDialog);
  const setOpen = useApp((s) => s.setLicenseDialog);
  const license = useGateway((s) => s.license);
  const activate = useGateway((s) => s.activateLicense);

  const [token, setToken] = useState('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState('');

  // 关掉即清场：下次打开不该看到上一次贴了一半的 key 和报错
  useEffect(() => {
    if (!open) {
      setToken('');
      setErr('');
      setBusy(false);
    }
  }, [open]);

  if (!open) return null;

  const replacing = !!license?.pro;

  const doActivate = async () => {
    if (!token.trim()) {
      setErr('请先粘贴许可证');
      return;
    }
    setBusy(true);
    setErr('');
    try {
      const l = await activate(token.trim());
      if (l.pro) {
        toast(`已激活：${l.subject ?? 'Pro'}`);
        setOpen(false);
      } else {
        // 签名能过但已过期/机器不符：留在这里让用户看到原因，不清输入
        setErr(l.message ?? '许可证未生效');
        setBusy(false);
      }
    } catch (e) {
      // 后端已经把「签名不对 / 绑了别的机器 / 已过期」分开了，
      // 这里原样透出，不要统一成「激活失败」——那等于把排查线索吞掉
      setErr(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  return (
    <Modal
      title={replacing ? <T zh="更换许可证" en="Change license" /> : <T zh="激活 Freeport Pro" en="Activate Freeport Pro" />}
      subtitle={
        <T
          zh="离线验签，不联网、不上传。激活失败不影响转发功能。"
          en="Verified offline — never uploaded. Activation never affects forwarding."
        />
      }
      onClose={() => setOpen(false)}
    >
      <textarea
        className="transfer-box license-box"
        value={token}
        autoFocus
        spellCheck={false}
        placeholder="eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9..."
        onChange={(e) => {
          setToken(e.target.value);
          setErr('');
        }}
        onKeyDown={(e) => {
          // textarea 里 Enter 要换行；Cmd/Ctrl+Enter 才是提交
          if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') void doActivate();
        }}
      />
      {err && <div className="license-note bad" style={{ margin: '8px 0 0' }}>{err}</div>}
      <div className="modal-foot">
        <span className="sub" style={{ marginRight: 'auto' }}>
          <T zh="本机安装码" en="Machine ID" />{' '}
          <span className="mono faint">{license?.machineId ?? '—'}</span>
        </span>
        <button className="btn ghost sm" onClick={() => setOpen(false)}>
          <T zh="取消" en="Cancel" />
        </button>
        <button className="btn sm" disabled={busy || !token.trim()} onClick={() => void doActivate()}>
          {busy ? <T zh="验证中…" en="Verifying…" /> : replacing ? <T zh="替换" en="Replace" /> : <T zh="激活" en="Activate" />}
        </button>
      </div>
    </Modal>
  );
}
