import { useEffect, useRef, useState, type FormEvent } from 'react'
import { Copy, ExternalLink, Globe } from 'lucide-react'
import {
  AlertDialog, Badge, Banner, Button, ButtonGroup, Collapsible, EmptyState, FormLayout,
  Heading, Icon, MetadataList, MetadataListItem, Selector, Stack, Table, TableBody,
  TableCell, TableHeader, TableHeaderCell, TableRow, Text, TextInput, useToast,
} from '@astryxdesign/core'
import { api } from './api'
import { DEFAULT_NGROK_SETTINGS, type NgrokSettings, type NgrokStatus, type ProxyRoute } from './types'

type Run = (label: string, work: () => Promise<void>, success?: string) => Promise<void> | undefined

const MANUAL_TARGET = '__manual__'
const errorText = (error: unknown) => error instanceof Error ? error.message : String(error)

function commandPreview(settings: NgrokSettings) {
  const parts = ['ngrok', 'http', settings.target.trim() || '本地地址']
  if (settings.publicUrl.trim()) parts.push('--url', settings.publicUrl.trim())
  if (settings.hostHeader.trim()) parts.push('--host-header', settings.hostHeader.trim())
  if (settings.trafficPolicyFile.trim()) parts.push('--traffic-policy-file', settings.trafficPolicyFile.trim())
  return parts.join(' ')
}

export function NgrokPage({
  desktop, disabled, busy, routes, run,
}: {
  desktop: boolean
  disabled: boolean
  busy: string | null
  routes: ProxyRoute[]
  run: Run
}) {
  const toast = useToast()
  const hydrated = useRef(false)
  const holdSave = useRef(false)
  const [status, setStatus] = useState<NgrokStatus | null>(null)
  const [form, setForm] = useState<NgrokSettings>(DEFAULT_NGROK_SETTINGS)
  const [token, setToken] = useState('')
  const [advanced, setAdvanced] = useState(false)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [confirmStop, setConfirmStop] = useState(false)
  const [preferManual, setPreferManual] = useState(false)
  const formRef = useRef(form)
  formRef.current = form

  useEffect(() => {
    if (!desktop) return
    let stop = false
    const pull = async () => {
      try {
        const next = await api.ngrokStatus()
        if (stop) return
        setStatus(next)
        setLoadError(null)
        if (!hydrated.current) {
          hydrated.current = true
          setForm(next.settings)
          if (next.settings.publicUrl || next.settings.hostHeader || next.settings.trafficPolicyFile) setAdvanced(true)
        }
      } catch (error) {
        if (!stop) setLoadError(errorText(error))
      }
    }
    void pull()
    const timer = window.setInterval(() => { void pull() }, 2000)
    return () => { stop = true; window.clearInterval(timer) }
  }, [desktop])

  useEffect(() => {
    if (!desktop || !hydrated.current || holdSave.current || status?.running) return
    const timer = window.setTimeout(() => {
      void api.ngrokSave(formRef.current).catch(() => {})
    }, 400)
    return () => window.clearTimeout(timer)
  }, [desktop, form, status?.running])

  const tunnel = status?.tunnels.find(item => item.publicUrl.startsWith('https://')) ?? status?.tunnels[0]
  const running = status?.running ?? false
  const fieldsLocked = disabled || running
  const matchedProjectId = routes.find(route => form.target === `http://localhost:${route.port}` || form.target === `http://127.0.0.1:${route.port}`)?.id ?? ''
  const projectId = preferManual ? MANUAL_TARGET : matchedProjectId || MANUAL_TARGET

  const copy = async (value: string) => {
    try { await navigator.clipboard.writeText(value); toast({ body: <span data-toast-status="success">已复制</span> }) }
    catch { toast({ body: '无法访问剪贴板，请选择并复制内容。', type: 'error' }) }
  }
  const patch = (partial: Partial<NgrokSettings>) => setForm(current => ({ ...current, ...partial }))
  const start = (event?: FormEvent) => {
    event?.preventDefault()
    if (fieldsLocked || !form.target.trim()) {
      document.getElementById('ngrok-target')?.focus()
      return
    }
    holdSave.current = true
    void run('正在启动隧道', async () => {
      try {
        const next = await api.ngrokStart(form)
        setStatus(next)
        setForm(next.settings)
      } finally { holdSave.current = false }
    }, '公网隧道已启动。')
  }
  const saveToken = (event: FormEvent) => {
    event.preventDefault()
    if (disabled || !token.trim()) {
      document.getElementById('ngrok-token')?.focus()
      return
    }
    void run('正在保存 ngrok 账号', async () => {
      await api.ngrokSetAuthtoken(token.trim())
      setToken('')
      setStatus(await api.ngrokStatus())
    }, 'ngrok 账号已保存到本机配置。')
  }

  const checking = desktop && !status && !loadError
  const detectionLabel = !desktop
    ? '打开桌面应用后检查'
    : checking
      ? '正在检查…'
      : status?.installed
        ? `已安装${status.version ? ` · ${status.version}` : ''}`
        : '未安装'

  return <Stack gap={6}>
    <Text>把本机正在运行的项目临时公开。这里执行的是 ngrok http，账号使用这台 Mac 上的 ngrok 配置。</Text>
    <MetadataList title="本机检测">
      <MetadataListItem label="ngrok">{detectionLabel}</MetadataListItem>
      {status?.binary && <MetadataListItem label="位置">{status.binary}</MetadataListItem>}
      {status?.installed && <MetadataListItem label="账号">{status.authtokenConfigured ? '已配置' : '未配置'}</MetadataListItem>}
    </MetadataList>
    <Text>关闭窗口后隧道会继续运行，回到此页可以停止。</Text>
    {loadError && <Banner status="error" title="暂时无法读取 ngrok 状态" description={loadError} />}
    {checking && <Banner status="info" title="正在检查本机是否安装了 ngrok" />}
    {status && !status.installed && <Banner status="error" title="未检测到 ngrok" description="先在终端安装：brew install --cask ngrok。安装后此页会自动再次检查。" endContent={<Button label="复制安装命令" icon={<Icon icon={Copy} />} onClick={() => { void copy('brew install --cask ngrok') }} />} />}
    {status?.installed && !status.authtokenConfigured && <form aria-label="ngrok 账号" onSubmit={saveToken}><FormLayout>
      <TextInput id="ngrok-token" label="Authtoken *" type="password" isRequired hasAutoCorrect={false} autoCapitalize="none" hasSpellCheck={false} value={token} onChange={setToken} isDisabled={disabled} description="从 ngrok 控制台复制。保存后写入本机 ngrok 配置，LanDNS 不会留存这份 token。" />
      <Button label="保存账号" type="submit" variant="primary" isDisabled={disabled || !token.trim()} isLoading={busy === '正在保存 ngrok 账号'} />
    </FormLayout></form>}
    {status?.installed && status.authtokenConfigured && <Collapsible trigger="更换 authtoken" defaultIsOpen={false}>
      <form aria-label="更换 ngrok 账号" onSubmit={saveToken}><FormLayout>
        <TextInput id="ngrok-token" label="新的 authtoken *" type="password" isRequired hasAutoCorrect={false} autoCapitalize="none" hasSpellCheck={false} value={token} onChange={setToken} isDisabled={disabled} />
        <Button label="更换账号" type="submit" isDisabled={disabled || !token.trim()} isLoading={busy === '正在保存 ngrok 账号'} />
      </FormLayout></form>
    </Collapsible>}
    {status?.message && <Banner status={running ? 'info' : 'error'} title={status.message} role="status" />}
    {running && !status?.owned && <Banner status="info" title="这台 Mac 上已经有 ngrok 在运行" description="它不是从 LanDNS 启动的。可以先使用当前地址，或停止后再从这里启动。" />}

    {running && tunnel ? <Stack gap={4}>
      <Stack direction="horizontal" gap={3} align="center" justify="between" wrap="wrap">
        <Heading level={2}>当前公网地址</Heading>
        <Badge variant="success" label={status?.owned ? '由 LanDNS 启动' : '已在运行'} />
      </Stack>
      <Text wordBreak="break-word">{tunnel.publicUrl}</Text>
      <Stack direction="horizontal" justify="end">
        <ButtonGroup label="公网地址">
          <Button label="复制地址" icon={<Icon icon={Copy} />} onClick={() => { void copy(tunnel.publicUrl) }} />
          <Button label="打开网址" variant="primary" icon={<Icon icon={ExternalLink} />} isDisabled={disabled} onClick={() => { void run('正在打开浏览器', () => api.openNgrokUrl(tunnel.publicUrl)) }} />
          {status?.inspectUrl && <Button label="查看请求" isDisabled={disabled} onClick={() => { void run('正在打开请求检查', () => api.openNgrokUrl(status.inspectUrl!)) }} />}
        </ButtonGroup>
      </Stack>
      {tunnel.addr && <MetadataList><MetadataListItem label="转发到">{tunnel.addr}</MetadataListItem></MetadataList>}
    </Stack> : running ? <EmptyState icon={<Icon icon={Globe} />} title="正在连接 ngrok" description="公网地址出现后会显示在这里。" /> : null}

    <form aria-label="ngrok 隧道" onSubmit={start}><FormLayout>
      {routes.length > 0 && <Selector label="使用本地项目" description="选择项目会填入它的本地地址。选「手动填写」后可以自己改。" value={projectId} onChange={id => {
        if (!id || id === MANUAL_TARGET) { setPreferManual(true); return }
        const route = routes.find(item => item.id === id)
        if (!route) return
        setPreferManual(false)
        patch({ target: `http://localhost:${route.port}` })
      }} isDisabled={fieldsLocked} options={[{ value: MANUAL_TARGET, label: '手动填写' }, ...routes.map(route => ({ value: route.id, label: `${route.name} · localhost:${route.port}` }))]} />}
      <TextInput id="ngrok-target" label="本地地址 *" isRequired inputMode="url" hasAutoCorrect={false} autoCapitalize="none" hasSpellCheck={false} placeholder="http://localhost:8080" value={form.target} onChange={value => { setPreferManual(true); patch({ target: value }) }} isDisabled={fieldsLocked} description="只转发本机地址。也可以只填端口，例如 8080。" />
      <Collapsible trigger="固定地址与请求头" isOpen={advanced} onOpenChange={setAdvanced}>
        <FormLayout>
          <TextInput id="ngrok-url" label="固定公网网址" inputMode="url" hasAutoCorrect={false} autoCapitalize="none" hasSpellCheck={false} placeholder="https://name.ngrok-free.dev" value={form.publicUrl} onChange={value => patch({ publicUrl: value })} isDisabled={fieldsLocked} description="留空时由 ngrok 分配地址。填写后对应 --url。" />
          <TextInput id="ngrok-host" label="Host 头" hasAutoCorrect={false} autoCapitalize="none" hasSpellCheck={false} placeholder="localhost:8080" value={form.hostHeader} onChange={value => patch({ hostHeader: value })} isDisabled={fieldsLocked} description="留空使用默认值。填写后对应 --host-header，也可填 rewrite。" />
          <TextInput id="ngrok-policy" label="流量策略文件" hasAutoCorrect={false} autoCapitalize="none" hasSpellCheck={false} placeholder="/绝对路径/policy.json" value={form.trafficPolicyFile} onChange={value => patch({ trafficPolicyFile: value })} isDisabled={fieldsLocked} description="对应 --traffic-policy-file。不需要时留空。" />
        </FormLayout>
      </Collapsible>
      <MetadataList><MetadataListItem label="将执行"><Text wordBreak="break-word">{commandPreview(form)}</Text></MetadataListItem></MetadataList>
      <Stack direction="horizontal" gap={3} wrap="wrap">
        <Button label="启动隧道" type="submit" variant="primary" isDisabled={fieldsLocked || !status?.installed || !status.authtokenConfigured} isLoading={busy === '正在启动隧道'} />
        {running && <Button label="停止隧道" type="button" variant="destructive" isDisabled={disabled} isLoading={busy === '正在停止隧道'} onClick={() => setConfirmStop(true)} />}
      </Stack>
    </FormLayout></form>

    <Stack gap={3}>
      <Heading level={2}>命令输出</Heading>
      {!status?.logs.length ? <EmptyState icon={<Icon icon={Globe} />} title="还没有输出" description="启动隧道后，在这里查看 ngrok 的记录。" /> : <div role="log" aria-label="ngrok 输出"><Table aria-label="ngrok 输出"><TableHeader><TableRow><TableHeaderCell scope="col">记录</TableHeaderCell></TableRow></TableHeader><TableBody>{status.logs.map((line, index) => <TableRow key={`${index}-${line.slice(0, 24)}`}><TableCell><Text wordBreak="break-word">{line}</Text></TableCell></TableRow>)}</TableBody></Table></div>}
    </Stack>

    <AlertDialog isOpen={confirmStop} onOpenChange={value => { if (!value && !disabled) setConfirmStop(false) }} title="停止当前 ngrok 隧道？" description={status?.owned ? '公网地址会立即失效。本地项目本身继续运行。' : '这会结束当前正在监听的 ngrok 进程。如果它是在终端里启动的，那个命令也会退出。'} cancelLabel="取消" actionLabel="停止隧道" isActionLoading={disabled} onAction={() => {
      if (disabled) return
      void run('正在停止隧道', async () => {
        setStatus(await api.ngrokStop())
        setConfirmStop(false)
      }, '隧道已停止。')
    }} />
  </Stack>
}
