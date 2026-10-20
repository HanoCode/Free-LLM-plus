import { useState } from 'react';
import { FileDown, FileUp, KeyRound, ShieldAlert } from 'lucide-react';
import { T } from '../lib/i18n';
import { useGateway } from '../lib/gateway';
import { CopyBtn, toast } from '../components/ui';
import type { ImportSummary } from '../lib/bridge';

/**
 * 迁移与备份（多机同步 v1）。
 *
 * 迁移码不是「配置导出」，是**一个装着全部 API key 的加密信封**：
 * 口令经 Argon2id 派生 AES-256-GCM 的密钥，口令不落盘、不上传。
 * 所以这一栏的措辞必须够重——它丢了等于把 key 发出去，它忘了等于数据没了。
 *
 * 两颗按钮的语义差别也要在界面上讲清楚：
 * - 合并：只补本机没有的厂商/凭据，绝不覆盖已有的 key
 * - 覆盖：整体替换，并清掉本机多余的密文（否则「删了还在」）
 */

const MIN_PASS = 8;

export function Transfer({ desktop }: { desktop: boolean }) {
  const exportBundle = useGateway((s) => s.exportBundle);
  const exportBundleFile = useGateway((s) => s.exportBundleFile);
  const importBundle = useGateway((s) => s.importBundle);
  const importBundleFile = useGateway((s) => s.importBundleFile);

  /* 导出 */
  const [pass, setPass] = useState('');
  const [again, setAgain] = useState('');
  const [blob, setBlob] = useState('');
  const [exporting, setExporting] = useState(false);
  const [saving, setSaving] = useState(false);

  /* 导入 */
  const [inBlob, setInBlob] = useState('');
  const [inPass, setInPass] = useState('');
  const [mode, setMode] = useState<'merge' | 'replace'>('merge');
  const [confirmed, setConfirmed] = useState(false);
  const [importing, setImporting] = useState(false);
  const [picking, setPicking] = useState(false);

  const [err, setErr] = useState('');

  /** 导入结果的统一说法（粘贴与选文件两条路共用） */
  const announce = (sum: ImportSummary) => {
    const kept =
      sum.providersKept + sum.accountsKept + sum.keysKept > 0
        ? `，已存在 ${sum.providersKept} 个厂商 / ${sum.accountsKept} 条凭据 / ${sum.keysKept} 把虚拟 key`
        : '';
    // 墓碑跳过要说出来：不然用户会以为「我删掉的厂商怎么又回来了」
    const buried =
      (sum.providersSkipped ?? 0) + (sum.accountsSkipped ?? 0) > 0
        ? `；${sum.providersSkipped} 个厂商 / ${sum.accountsSkipped} 条凭据是你在这台机器上删过的，没有导入`
        : '';
    toast(
      `导入完成：+${sum.providersAdded} 厂商 · +${sum.accountsAdded} 凭据 · +${sum.keysAdded} 虚拟 key${kept}${buried}`,
    );
  };

  const doExport = async () => {
    setErr('');
    if (pass.length < MIN_PASS) {
      return setErr(`口令至少 ${MIN_PASS} 位——它保护的是你全部的 API key`);
    }
    if (pass !== again) return setErr('两次输入的口令不一致');
    setExporting(true);
    try {
      const out = await exportBundle(pass);
      setBlob(out);
      // 口令留在输入框里没有意义，反而会被截屏
      setPass('');
      setAgain('');
      toast('迁移码已生成，复制后像密码一样保管');
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setExporting(false);
    }
  };

  /** 存成 .fpk：和「生成迁移码」同一份内容，只是不再需要用户自己找地方粘贴 */
  const doExportFile = async () => {
    setErr('');
    if (pass.length < MIN_PASS) {
      return setErr(`口令至少 ${MIN_PASS} 位——它保护的是你全部的 API key`);
    }
    if (pass !== again) return setErr('两次输入的口令不一致');
    setSaving(true);
    try {
      const path = await exportBundleFile(pass);
      if (!path) return; // 用户取消对话框：不是错误
      setPass('');
      setAgain('');
      setBlob('');
      toast(`已保存：${path}`);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const doImport = async () => {
    setErr('');
    if (!inBlob.trim()) return setErr('先粘贴迁移码');
    if (!inPass) return setErr('请输入迁移码的口令');
    if (mode === 'replace' && !confirmed) {
      return setErr('覆盖会清掉本机现有凭据，请先勾选确认');
    }
    setImporting(true);
    try {
      const sum = await importBundle(inBlob.trim(), inPass, mode);
      announce(sum);
      setInBlob('');
      setInPass('');
      setConfirmed(false);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setImporting(false);
    }
  };

  /** 从 .fpk 导入：路径由原生对话框给，前端不碰文件系统 */
  const doImportFile = async () => {
    setErr('');
    if (!inPass) return setErr('请先填写这个迁移包的口令');
    if (mode === 'replace' && !confirmed) {
      return setErr('覆盖会清掉本机现有凭据，请先勾选确认');
    }
    setPicking(true);
    try {
      const sum = await importBundleFile(inPass, mode);
      if (!sum) return;
      announce(sum);
      setInPass('');
      setInBlob('');
      setConfirmed(false);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setPicking(false);
    }
  };

  return (
    <div className="card">
      <div className="card-head">
        <h3>
          <T zh="迁移与备份" en="Migrate & back up" />
        </h3>
        <span className="sub">
          <T zh="口令加密，换机不用重填 key" en="passphrase-encrypted, no re-entering keys" />
        </span>
      </div>
      <div className="card-body">
        <div className="hint-line" style={{ marginTop: 0 }}>
          <ShieldAlert size={12} style={{ verticalAlign: -2, marginRight: 4 }} />
          <T
            zh="迁移码里是所有 API key 的密文。口令丢了就解不开，我们也没有后门；迁移码泄露 + 口令泄露 = key 泄露。"
            en="The code holds your API keys in ciphertext. Lose the passphrase and it's gone — there is no back door. Leaked code plus leaked passphrase equals leaked keys."
          />
        </div>

        <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 10, marginTop: 12 }}>
          <div className="field">
            <label>
              <T zh="导出 · 设置口令（≥ 8 位）" en="Export · passphrase (8+ chars)" />
            </label>
            <input
              type="password"
              value={pass}
              autoComplete="new-password"
              onChange={(e) => setPass(e.target.value)}
              placeholder="••••••••"
            />
          </div>
          <div className="field">
            <label>
              <T zh="再输一次" en="Confirm" />
            </label>
            <input
              type="password"
              value={again}
              autoComplete="new-password"
              onChange={(e) => setAgain(e.target.value)}
            />
          </div>
        </div>

        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <button
            className="btn primary sm"
            disabled={!desktop || exporting}
            onClick={() => void doExport()}
          >
            <KeyRound size={12} />
            {exporting ? (
              <T zh="正在派生密钥…" en="Deriving key…" />
            ) : (
              <T zh="生成迁移码" en="Generate code" />
            )}
          </button>
          <button
            className="btn ghost sm"
            disabled={!desktop || saving}
            onClick={() => void doExportFile()}
            title="保存成 .fpk 文件，换机时直接选文件导入"
          >
            <FileDown size={12} />
            {saving ? (
              <T zh="正在写入…" en="Writing…" />
            ) : (
              <T zh="存成 .fpk 文件" en="Save as .fpk" />
            )}
          </button>
          {blob && <CopyBtn value={blob} label="复制迁移码" />}
          {blob && (
            <span className="sub" style={{ marginLeft: 'auto' }}>
              {blob.length.toLocaleString()} <T zh="字符" en="chars" />
            </span>
          )}
        </div>

        {blob && (
          <div className="field" style={{ marginTop: 10 }}>
            <label>
              <T
                zh="迁移码（整段复制，存进密码管理器）"
                en="Transfer code (copy the whole block)"
              />
            </label>
            <textarea
              className="transfer-box"
              readOnly
              value={blob}
              onFocus={(e) => e.currentTarget.select()}
            />
          </div>
        )}

        <div className="transfer-split">
          <span>
            <T zh="从另一台机器导入" en="Import from another machine" />
          </span>
        </div>

        <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 10 }}>
          <button
            className="btn sm"
            disabled={!desktop || picking}
            onClick={() => void doImportFile()}
          >
            <FileUp size={12} />
            {picking ? (
              <T zh="正在读取…" en="Reading…" />
            ) : (
              <T zh="选择 .fpk 文件" en="Choose a .fpk" />
            )}
          </button>
          <span className="sub">
            <T
              zh="直接选文件，不用打开文本编辑器。口令仍然要填在下面。"
              en="Pick the file directly — no text editor needed. You still enter the passphrase below."
            />
          </span>
        </div>

        <div className="field">
          <label>
            <T zh="或者粘贴迁移码" en="Or paste transfer code" />
          </label>
          <textarea
            className="transfer-box"
            value={inBlob}
            placeholder='{"format":"freeport.transfer", ...}'
            onChange={(e) => setInBlob(e.target.value)}
          />
        </div>

        <div style={{ display: 'grid', gridTemplateColumns: '1fr 200px', gap: 10 }}>
          <div className="field">
            <label>
              <T zh="迁移码的口令" en="Passphrase" />
            </label>
            <input
              type="password"
              value={inPass}
              autoComplete="off"
              onChange={(e) => setInPass(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && !importing && void doImport()}
            />
          </div>
          <div className="field">
            <label>
              <T zh="方式" en="Mode" />
            </label>
            <select
              className="select"
              value={mode}
              onChange={(e) => {
                setMode(e.target.value as 'merge' | 'replace');
                setConfirmed(false);
              }}
            >
              <option value="merge">
                <T zh="合并（只补缺失）" en="Merge (fill gaps)" />
              </option>
              <option value="replace">
                <T zh="覆盖（整体替换）" en="Replace (everything)" />
              </option>
            </select>
          </div>
        </div>

        {mode === 'replace' && (
          <label className="transfer-warn">
            <input
              type="checkbox"
              checked={confirmed}
              onChange={(e) => setConfirmed(e.target.checked)}
            />
            <span>
              <T
                zh="我确认：本机现有的厂商与凭据会被替换，多余的密文会被删除。"
                en="I understand: local providers and credentials will be replaced, extra ciphertext deleted."
              />
            </span>
          </label>
        )}

        {err && <div className="modal-err">{err}</div>}

        <div style={{ display: 'flex', gap: 8, marginTop: 12 }}>
          <button
            className="btn sm"
            disabled={!desktop || importing}
            onClick={() => void doImport()}
          >
            {importing ? (
              <T zh="正在解密…" en="Decrypting…" />
            ) : (
              <T zh="导入" en="Import" />
            )}
          </button>
          <span className="sub" style={{ alignSelf: 'center' }}>
            <T
              zh="合并模式不会覆盖同名凭据的 key；同一把虚拟 key 迁移后原客户端继续可用。"
              en="Merge never overwrites an existing credential; migrated virtual keys keep working for existing clients."
            />
          </span>
        </div>
      </div>
    </div>
  );
}
