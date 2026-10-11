import { AlertTriangle, FileSpreadsheet, LoaderCircle, LockKeyhole, Settings2, Sparkles, X } from "lucide-react";
import { useEffect, useState } from "react";

import {
  createCommunicationReport,
  getSummarySettings,
  testSummaryEndpoint,
  updateSummarySettings,
  type CommunicationReportResult,
  type SummarySettings,
} from "../lib/server-api";

function currentMonth(): string {
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}`;
}

export function CommunicationReportDialog({ token, onClose, onCompleted }: {
  token: string;
  onClose: () => void;
  onCompleted: (result: CommunicationReportResult) => void;
}) {
  const [month, setMonth] = useState(currentMonth());
  const [startDate, setStartDate] = useState("");
  const [endDate, setEndDate] = useState("");
  const [dataRedaction, setDataRedaction] = useState(true);
  // Personal chats are what most reports are about, so this starts on.
  const [onlyDirect, setOnlyDirect] = useState(true);
  // WeCom's own external-contact list is what separates a customer from a
  // colleague sharing the same nickname, so this also starts on.
  const [onlyExternal, setOnlyExternal] = useState(true);
  const [summarize, setSummarize] = useState(false);
  const [settings, setSettings] = useState<SummarySettings>();
  const [showSettings, setShowSettings] = useState(false);
  const [endpoint, setEndpoint] = useState("");
  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [clearApiKey, setClearApiKey] = useState(false);
  const [prompt, setPrompt] = useState("");
  // 独立于「开启总结」的开关：图片是否随正文一起发给模型，默认开。
  const [attachMedia, setAttachMedia] = useState(true);
  const [testResult, setTestResult] = useState<{ ok: boolean; detail: string }>();
  const [testing, setTesting] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  useEffect(() => {
    let cancelled = false;
    void getSummarySettings(token)
      .then((loaded) => {
        if (cancelled) return;
        setSettings(loaded);
        setEndpoint(loaded.endpoint);
        setModel(loaded.model);
        setPrompt(loaded.prompt ?? "");
        setAttachMedia(loaded.attachMedia ?? true);
      })
      .catch(() => undefined);
    return () => { cancelled = true; };
  }, [token]);

  const summarizationReady = Boolean(settings?.enabled && settings.hasApiKey && settings.endpoint && settings.model && settings.prompt);

  const saveSettings = async () => {
    setBusy(true);
    setError(undefined);
    try {
      const payload: Parameters<typeof updateSummarySettings>[1] = { enabled: true, endpoint, model, prompt, attachMedia };
      if (clearApiKey) {
        // An empty string is the documented way to erase the stored key; leaving
        // the field blank would otherwise keep whatever is already there.
        payload.apiKey = "";
      } else if (apiKey.trim()) {
        payload.apiKey = apiKey.trim();
      }
      const saved = await updateSummarySettings(token, payload);
      setSettings(saved);
      setApiKey("");
      setShowSettings(false);
      setSummarize(true);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "保存大模型设置失败。");
    } finally {
      setBusy(false);
    }
  };

  const runTest = async () => {
    // Without the loaded state the fields are still blank, and the save that
    // precedes every probe would overwrite a working configuration with it.
    if (!settings) {
      setError("配置还没读取完成，请稍候再试。");
      return;
    }
    setTesting(true);
    setError(undefined);
    try {
      // Save first so the probe uses exactly what is currently in the form.
      const payload: Parameters<typeof updateSummarySettings>[1] = { enabled: settings?.enabled ?? false, endpoint, model, prompt, attachMedia };
      if (clearApiKey) {
        payload.apiKey = "";
      } else if (apiKey.trim()) {
        payload.apiKey = apiKey.trim();
      }
      const saved = await updateSummarySettings(token, payload);
      setApiKey("");
      // Keep the form state in sync: a key typed into this run is stored now,
      // and the placeholder must stop claiming the settings are unsaved.
      setSettings(saved);
      setTestResult(await testSummaryEndpoint(token));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "测试连接失败。");
    } finally {
      setTesting(false);
    }
  };

  const disableSummarization = async () => {
    setBusy(true);
    setError(undefined);
    try {
      const saved = await updateSummarySettings(token, { enabled: false, endpoint, model });
      setSettings(saved);
      setSummarize(false);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "关闭大模型总结失败。");
    } finally {
      setBusy(false);
    }
  };

  const submit = async () => {
    setBusy(true);
    setError(undefined);
    try {
      const result = await createCommunicationReport(token, {
        month: month.trim() || undefined,
        startDate: startDate || undefined,
        endDate: endDate || undefined,
        dataRedaction,
        onlyDirect,
        onlyExternal,
        summarize,
      });
      onCompleted(result);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "沟通记录导出失败。");
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={busy ? undefined : onClose}>
      <section className="modal-card export-dialog-card" role="dialog" aria-modal="true" aria-labelledby="communication-report-title" onMouseDown={(event) => event.stopPropagation()}>
        <button className="modal-close" type="button" aria-label="关闭沟通记录弹窗" onClick={onClose} disabled={busy}><X size={18} /></button>
        <span className="modal-icon"><FileSpreadsheet /></span>
        <h2 id="communication-report-title">生成沟通记录</h2>
        <p>按「姓名 + 月份」汇总：一人一月一行，含该月全部聊天记录和总结。媒体原文件复制到导出目录。</p>

        <div className="export-dialog-options">
          <fieldset>
            <legend>时间范围</legend>
            <label>按月份<input type="month" value={month} onChange={(event) => setMonth(event.target.value)} /></label>
            <small>留空月份可改用下面的起止日期；两者都填时以月份为准。</small>
            <div className="export-date-range">
              <label>开始日期<input type="date" value={startDate} onChange={(event) => setStartDate(event.target.value)} /></label>
              <label>结束日期<input type="date" value={endDate} onChange={(event) => setEndDate(event.target.value)} /></label>
            </div>
          </fieldset>

          <fieldset>
            <legend>会话范围</legend>
            <label>
              <input type="checkbox" checked={onlyDirect} onChange={(event) => setOnlyDirect(event.target.checked)} />
              仅个人会话（排除群聊）
            </label>
            <label>
              <input type="checkbox" checked={onlyExternal} onChange={(event) => setOnlyExternal(event.target.checked)} />
              仅外部客户（排除同事）
            </label>
            <small>「外部客户」按企业微信自己的外部联系人名单判定，与同事重名的联系人也能区分开。</small>
          </fieldset>

          <fieldset>
            <legend>输出内容</legend>
            <label><input type="checkbox" checked={dataRedaction} onChange={(event) => setDataRedaction(event.target.checked)} />数据脱敏</label>
            <label>
              <input type="checkbox" checked={summarize} disabled={!summarizationReady} onChange={(event) => setSummarize(event.target.checked)} />
              大模型自动总结
            </label>
            <small>「聊天记录」列直接写入该月的完整记录，不依赖大模型；超过 Excel 单元格上限时自动截断，完整内容见沟通记录目录。</small>
            {!summarizationReady && (
              <small>需要先配置并开启大模型接口（含总结提示词），未开启时总结列留空，可手工填写。</small>
            )}
          </fieldset>

          <fieldset>
            <legend>大模型接口</legend>
            {settings?.enabled ? (
              <p className="summary-status">
                <LockKeyhole size={14} />
                已开启：{settings.model} @ {settings.endpoint}
              </p>
            ) : (
              <p className="summary-status">未开启。聊天内容不会被发送到任何外部服务。</p>
            )}
            <button className="secondary-button" type="button" onClick={() => setShowSettings((value) => !value)} disabled={busy}>
              <Settings2 size={15} />{showSettings ? "收起配置" : "配置大模型"}
            </button>
            {showSettings && (
              <div className="summary-settings">
                <label>接口地址<input type="text" value={endpoint} onChange={(event) => setEndpoint(event.target.value)} placeholder="例如 https://api.example.com/v1" spellCheck={false} /><small>填到 /v1、/api/paas/v4 这类版本路径即可，程序会自动补全 chat/completions 路径；已填完整路径的不会改动。填好后可先点「测试连接」，无需先保存开启。</small></label>
                <label>模型名称<input type="text" value={model} onChange={(event) => setModel(event.target.value)} placeholder="例如 gpt-4o-mini" spellCheck={false} /></label>
                <label>API 密钥<input type="password" value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder={settings?.hasApiKey ? "已保存，留空表示不修改" : "尚未保存"} autoComplete="off" />
                  {settings?.hasApiKey && <label className="inline-check"><input type="checkbox" checked={clearApiKey} onChange={(event) => setClearApiKey(event.target.checked)} />清除已保存的密钥</label>}
                </label>
                <label className="inline-check">
                  <input type="checkbox" checked={attachMedia} onChange={(event) => setAttachMedia(event.target.checked)} />
                  总结时附带图片和语音（每段会话各自的媒体随正文一起发送，语音在本地解码为 wav，可单独关闭）
                </label>
                <label>总结提示词<textarea value={prompt} onChange={(event) => setPrompt(event.target.value)} placeholder="必填，没有内置提示词。例如：按「核心议题 / 结论 / 待办（责任人、时间）/ 风险 / 关键数据」五段输出。" rows={4} /><small>这段提示词作为系统提示词发送，紧跟其后的是这一个客户的完整聊天记录（含其图片与语音）。</small></label>
                <p className="summary-warning"><AlertTriangle size={14} />开启后，该会话范围内的聊天正文（勾选附带媒体时还包括图片和语音）会发送到你填写的接口地址。密钥使用 Windows DPAPI 加密保存在本机。</p>
                <button className="secondary-button" type="button" onClick={() => void runTest()} disabled={testing || busy}>
                  {testing ? <LoaderCircle className="spin" size={15} /> : <Settings2 size={15} />}测试连接
                </button>
                {testResult && (
                  <p className={testResult.ok ? "summary-status ok" : "summary-status bad"}>
                    {testResult.ok ? "✓ " : "✗ "}{testResult.detail}
                  </p>
                )}
                <div className="modal-actions">
                  {settings?.enabled && <button className="secondary-button" type="button" onClick={() => void disableSummarization()} disabled={busy}>关闭总结</button>}
                  <button className="primary-button" type="button" onClick={() => void saveSettings()} disabled={busy || !endpoint.trim() || !model.trim() || !prompt.trim()}>
                    {busy ? <LoaderCircle className="spin" size={15} /> : <Sparkles size={15} />}保存并开启
                  </button>
                </div>
              </div>
            )}
          </fieldset>
        </div>

        {error && <div className="inline-error"><AlertTriangle size={15} />{error}</div>}

        <div className="modal-actions">
          <button className="secondary-button" type="button" onClick={onClose} disabled={busy}>取消</button>
          <button className="primary-button" type="button" onClick={() => void submit()} disabled={busy}>
            {busy ? "正在生成…" : "生成沟通记录"}
          </button>
        </div>
      </section>
    </div>
  );
}