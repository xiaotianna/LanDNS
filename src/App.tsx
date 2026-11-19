import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react'
import {
  ArrowUpRight, Copy, ExternalLink, Globe, Laptop, LayoutDashboard, Plus,
  RefreshCw, Search, Settings2, TerminalSquare, Trash2,
} from 'lucide-react'
import {
  AlertDialog, AppShell, Badge, Banner, Button, ButtonGroup, Card, Collapsible, Dialog, DialogHeader,
  EmptyState, FormLayout, Heading, Icon, IconButton, Layout, LayoutContent, LayoutFooter,
  LayoutHeader, MetadataList, MetadataListItem, NumberInput, ProgressBar, Selector,
  SideNav, SideNavHeading, SideNavItem, SideNavSection, Spinner, Stack, Switch, Table,
  TableBody, TableCell, TableHeader, TableHeaderCell, TableRow, Text, TextInput, useToast,
} from '@astryxdesign/core'
import type { ThemeMode } from '@astryxdesign/core/theme'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { getVersion } from '@tauri-apps/api/app'
import { check, type Update } from '@tauri-apps/plugin-updater'
import { api, isDesktop } from './api'
import { DEFAULT_CONFIG, type Config, type DiagnosticResult, type ProxyRoute, type Snapshot } from './types'
import { getProjectUrl, parseLocalAddress, parseProjectAddress } from './lib/projects'
import { NgrokPage } from './NgrokPage'

type Page = 'overview' | 'proxy' | 'ngrok' | 'devices' | 'logs' | 'settings'
const pages = [
  { id: 'overview' as const, label: '概览', icon: LayoutDashboard },
  { id: 'proxy' as const, label: '本地项目', icon: ArrowUpRight },
  { id: 'ngrok' as const, label: '公网访问', icon: Globe },
  { id: 'devices' as const, label: '连接其他设备', icon: Laptop },
  { id: 'logs' as const, label: '运行日志', icon: TerminalSquare },
]
const copyConfig = (config: Config): Config => ({ ...config, routes: config.routes.map(route => ({ ...route })) })
const errorText = (error: unknown) => error instanceof Error ? error.message : String(error)
const displayTime = (timestamp: number) => new Date(timestamp * 1000).toLocaleTimeString('zh-CN', { hour12: false })
const number = new Intl.NumberFormat('zh-CN')

export default function App({ themeMode, onThemeChange }: { themeMode: ThemeMode; onThemeChange: (mode: ThemeMode) => void }) {
  const [page, setPage] = useState<Page>('overview')
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null)
  const [config, setConfig] = useState<Config>(() => copyConfig(DEFAULT_CONFIG))
  const [dirty, setDirty] = useState(false)
  const dirtyRef = useRef(false)
  const hadOverlay = useRef(false)
  const [busy, setBusy] = useState<string | null>(null)
  const toast = useToast()
  const [connectionError, setConnectionError] = useState<string | null>(null)
  const [loaded, setLoaded] = useState(!isDesktop)
  const [diagnostic, setDiagnostic] = useState<DiagnosticResult | null>(null)
  const [appVersion, setAppVersion] = useState('')
  const [updaterAvailable, setUpdaterAvailable] = useState(false)
  const [availableUpdate, setAvailableUpdate] = useState<Update | null>(null)
  const [updateInstalled, setUpdateInstalled] = useState(false)
  const [updateStatus, setUpdateStatus] = useState('')
  const [updateError, setUpdateError] = useState<string | null>(null)
  const [updateProgress, setUpdateProgress] = useState<number | null>(null)
  const updateServicePending = useRef(false)
  const [hostname, setHostname] = useState(DEFAULT_CONFIG.domain)
  const [routeEditor, setRouteEditor] = useState<ProxyRoute | null>(null)
  const [localAddress, setLocalAddress] = useState('')
  const [projectAddress, setProjectAddress] = useState('')
  const projectAddressEdited = useRef(false)
  const [selectedProjectId, setSelectedProjectId] = useState('')
  const [routeError, setRouteError] = useState<{ field: 'url' | 'address' | 'name'; message: string } | null>(null)
  const [filter, setFilter] = useState('')
  const [advancedOpen, setAdvancedOpen] = useState(false)
  const [settingsError, setSettingsError] = useState<{ field: 'domain' | 'upstream' | 'ttl'; message: string } | null>(null)
  const ttlEntryError = useRef<string | null>(null)
  const [confirm, setConfirm] = useState<{ kind: 'uninstall' } | { kind: 'route'; route: ProxyRoute } | null>(null)
  const running = snapshot?.service.running ?? false
  const serviceActive = snapshot?.serviceLoaded ?? false
  const network = snapshot?.network
  const currentPage = page === 'settings' ? { label: '设置' } : pages.find(item => item.id === page)!
  const currentConfig = snapshot?.config ?? DEFAULT_CONFIG
  const locked = Boolean(busy)

  const changeTheme = (value: string) => {
    if (value !== 'system' && value !== 'light' && value !== 'dark') return
    onThemeChange(value)
    try {
      if (value === 'system') localStorage.removeItem('dns-x-theme')
      else localStorage.setItem('dns-x-theme', value)
    } catch {
      toast({ body: '主题已切换，但无法保存。下次打开时可能恢复默认。', type: 'error' })
    }
  }

  useEffect(() => {
    if (!isDesktop) return
    void getCurrentWindow().setTheme(themeMode === 'system' ? null : themeMode).catch(error => {
      toast({ body: `无法切换窗口主题：${errorText(error)}`, type: 'error', uniqueID: 'window-theme' })
    })
  }, [themeMode, toast])

  const refresh = useCallback(async () => {
    if (!isDesktop) return
    try {
      const next = await api.snapshot()
      setSnapshot(next)
      setConnectionError(null)
      if (!dirtyRef.current) setConfig(copyConfig(next.config))
    } catch (error) {
      setConnectionError(errorText(error))
    } finally { setLoaded(true) }
  }, [])

  useEffect(() => {
    void refresh()
    const timer = window.setInterval(() => { void refresh() }, 4000)
    return () => window.clearInterval(timer)
  }, [refresh])

  useEffect(() => {
    if (!isDesktop) return
    const listener = listen<string>('tray-open', event => {
      if (event.payload === 'proxy') setPage('proxy')
      else if (event.payload === 'ngrok' || event.payload === 'ngrok-machine') setPage('ngrok')
    })
    return () => { void listener.then(unlisten => unlisten()) }
  }, [])

  useEffect(() => {
    if (isDesktop) void Promise.all([getVersion(), api.updaterAvailable()]).then(([version, available]) => {
      setAppVersion(version)
      setUpdaterAvailable(available)
    }).catch(error => setUpdateError(errorText(error)))
  }, [])
  useEffect(() => () => { void availableUpdate?.close().catch(() => {}) }, [availableUpdate])

  const change = (patch: Partial<Config>) => {
    setSettingsError(null)
    dirtyRef.current = true
    setDirty(true)
    setConfig(previous => ({ ...previous, ...patch }))
  }
  const action = async (label: string, work: () => Promise<void>, success?: string) => {
    if (busy) return
    setBusy(label)
    const dismissLoading = toast({ body: <span data-toast-status="loading">{label}</span>, endContent: <Spinner shade="inherit" aria-hidden="true" />, isAutoHide: false, uniqueID: 'service-action' })
    try {
      await work()
      await refresh()
      if (success) toast({ body: <span data-toast-status="success">{success}</span>, uniqueID: 'service-action' })
      else dismissLoading()
    } catch (error) { toast({ body: errorText(error), type: 'error', uniqueID: 'service-action' }) }
    finally { setBusy(null) }
  }
  const saved = () => { dirtyRef.current = false; setDirty(false) }
  const checkForUpdates = () => {
    if (!updaterAvailable || updateInstalled) return
    return action('正在检查更新', async () => {
      setUpdateError(null)
      setUpdateStatus('')
      setAvailableUpdate(null)
      try {
        const update = await check({ timeout: 30000 })
        setAvailableUpdate(update)
        setUpdateStatus(update ? `发现新版本 ${update.version}` : '当前已是最新版本。')
      } catch (error) {
        setUpdateError(`检查更新失败：${errorText(error)}`)
        throw error
      }
    })
  }
  const installUpdate = () => {
    if (!updaterAvailable || !availableUpdate || dirty || !loaded || connectionError) return
    return action('正在更新应用', async () => {
      setUpdateError(null)
      let installed = updateInstalled
      try {
        if (!installed) {
          updateServicePending.current = serviceActive
          setUpdateStatus('正在下载更新')
          setUpdateProgress(null)
          let downloaded = 0
          let total = 0
          await availableUpdate.downloadAndInstall(event => {
            if (event.event === 'Started') total = event.data.contentLength ?? 0
            else if (event.event === 'Progress') {
              downloaded += event.data.chunkLength
              if (total > 0) setUpdateProgress(Math.min(100, Math.floor(downloaded / total * 100)))
            } else {
              setUpdateStatus('正在校验并安装更新')
              setUpdateProgress(null)
            }
          }, { timeout: 300000 })
          installed = true
          setUpdateInstalled(true)
        }
        if (updateServicePending.current) {
          setUpdateStatus('正在更新共享服务，请完成 macOS 授权')
          // macOS installs the new bundle at the same path; startService copies its new executable.
          await api.startService(currentConfig)
          updateServicePending.current = false
        }
        setUpdateStatus('更新已安装，正在重启应用')
        await api.restartApp()
      } catch (error) {
        setUpdateStatus('')
        setUpdateError(`${installed ? '新版应用已安装，请重试完成更新' : '更新失败，可重试'}：${errorText(error)}`)
        throw error
      }
    })
  }
  const validateSettings = () => {
    let error: { field: 'domain' | 'upstream' | 'ttl'; message: string } | null = null
    if (config.domain.length > 253 || !config.domain.split('.').every(label => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label)) || !URL.canParse(`http://project.${config.domain}`)) {
      error = { field: 'domain', message: '请填写有效的网址后缀，例如 dev.test；每段使用小写字母、数字或连字符，连字符不能放在首尾。' }
    } else if (!config.upstreamDns.trim()) {
      error = { field: 'upstream', message: '请填写 DNS 地址，例如 1.1.1.1。' }
    } else if (ttlEntryError.current || !Number.isInteger(config.ttl) || config.ttl < 5 || config.ttl > 300) {
      error = { field: 'ttl', message: '请填写 5 到 300 之间的整数秒数。' }
    }
    if (!error) return true
    setSettingsError(error)
    if (routeEditor) return false
    setPage('settings')
    setAdvancedOpen(true)
    requestAnimationFrame(() => document.getElementById(error.field)?.focus())
    return false
  }
  const save = () => {
    if (!validateSettings()) return
    return action('正在保存配置', async () => { await api.saveConfig(config); saved() }, running ? '配置已保存，服务已应用更新。' : '配置已保存。')
  }
  const start = () => {
    if (!validateSettings()) return
    return action('正在启动服务', async () => { await api.startService(config); saved() }, '共享服务已启动。项目需要继续在这台 Mac 上运行。')
  }
  const stop = () => action('正在停止服务', () => api.stopService(), '服务已停止，本机 DNS 配置及 LanDNS 新增的代理绕过规则已移除。')
  const copy = async (value: string) => {
    try { await navigator.clipboard.writeText(value); toast({ body: <span data-toast-status="success">已复制 {value}</span> }) }
    catch { toast({ body: '无法访问剪贴板，请选择并复制地址。', type: 'error' }) }
  }
  const testDns = (name: string) => action('正在检查解析', async () => {
    setDiagnostic(null)
    setDiagnostic(await api.diagnose(name.trim()))
  })
  const discard = () => {
    setConfig(copyConfig(snapshot?.config ?? DEFAULT_CONFIG))
    setRouteEditor(null)
    setSettingsError(null)
    ttlEntryError.current = null
    saved()
  }
  const beginProject = (route?: ProxyRoute) => {
    if (!validateSettings()) return
    setSettingsError(null)
    setRouteEditor(route ? { ...route } : { id: crypto.randomUUID(), name: '', subdomain: '', port: 3000, enabled: true })
    setLocalAddress(route ? `http://localhost:${route.port}` : '')
    setProjectAddress(route ? route.subdomain === '*' && !route.hostname ? '*' : getProjectUrl(route, config) : '')
    projectAddressEdited.current = false
    setRouteError(null)
  }
  const submitRoute = (event: FormEvent) => {
    event.preventDefault()
    if (!routeEditor || locked || !isDesktop || !loaded || connectionError) return
    const reject = (field: 'url' | 'address' | 'name', message: string) => {
      setRouteError({ field, message })
      document.getElementById(`route-${field}`)?.focus()
    }
    const target = parseLocalAddress(localAddress)
    if ('error' in target) { reject('address', target.error); return }
    if (!validateSettings()) return
    const address = parseProjectAddress(projectAddress, config.domain)
    if ('error' in address) { reject('url', address.error); return }
    const candidate = { ...routeEditor, ...address, name: routeEditor.name.trim(), port: target.port, enabled: true }
    if (!projectAddressEdited.current && !routeEditor.hostname && address.hostname === `${routeEditor.subdomain}.${config.domain}`) {
      candidate.hostname = null
      candidate.subdomain = routeEditor.subdomain
    }
    const candidateHost = candidate.hostname ?? `${candidate.subdomain}.${config.domain}`
    if (config.routes.some(route => route.id !== candidate.id && (route.hostname ?? `${route.subdomain}.${config.domain}`) === candidateHost)) {
      reject('url', '这个访问网址已被其他项目使用，请换一个域名。'); return
    }
    if (candidate.name.length > 80) { reject('name', '项目备注最多 80 个字符。'); return }
    if (!candidate.name) candidate.name = candidate.subdomain === '*' ? '默认项目' : candidate.subdomain.slice(0, 80)
    const nextConfig = { ...config, proxyEnabled: true, routes: config.routes.some(route => route.id === candidate.id) ? config.routes.map(route => route.id === candidate.id ? candidate : route) : [...config.routes, candidate] }
    void action('正在保存并启用项目', async () => {
      await api.startService(nextConfig)
      setConfig(copyConfig(nextConfig))
      saved()
      setSelectedProjectId(candidate.id)
      setRouteEditor(null)
      setRouteError(null)
      setPage('overview')
    }, '已启用，可点击「打开网址」访问。')
  }
  const updateRoute = (id: string, enabled: boolean) => change({ proxyEnabled: enabled || config.proxyEnabled, routes: config.routes.map(route => route.id === id ? { ...route, enabled } : route) })
  const filteredRoutes = config.routes.filter(route => `${route.name} ${getProjectUrl(route, config)} ${route.port}`.toLowerCase().includes(filter.toLowerCase()))
  const enabledRoutes = currentConfig.routes.filter(route => route.enabled).length
  const activeDomain = currentConfig.domain
  const project = currentConfig.routes.find(route => route.id === selectedProjectId)
    ?? currentConfig.routes.find(route => route.enabled && route.subdomain !== '*')
    ?? currentConfig.routes.find(route => route.enabled)
    ?? currentConfig.routes[0]
  const projectUrl = project ? getProjectUrl(project, currentConfig) : ''
  const projectEnabled = Boolean(running && currentConfig.proxyEnabled && project?.enabled)
  const enableProject = () => {
    if (!validateSettings()) return
    return action('正在启用项目访问', async () => {
      const nextConfig = { ...config, proxyEnabled: true, routes: config.routes.map(route => route.id === project?.id ? { ...route, enabled: true } : route) }
      await api.startService(nextConfig)
      setConfig(copyConfig(nextConfig))
      saved()
    }, '项目转发已启用，请保持本地项目运行。')
  }
  const open = (url: string) => action('正在打开浏览器', () => api.openUrl(url))
  useEffect(() => { setHostname(projectUrl ? new URL(projectUrl).hostname : activeDomain); setDiagnostic(null) }, [projectUrl, activeDomain])
  useEffect(() => { document.title = `${currentPage.label} · LanDNS` }, [currentPage.label])
  useEffect(() => { setRouteError(null) }, [routeEditor?.id])
  useEffect(() => {
    const overlayOpen = Boolean(confirm || routeEditor)
    if (hadOverlay.current && !overlayOpen) {
      requestAnimationFrame(() => {
        if (document.activeElement === document.body) document.querySelector<HTMLElement>('[role="main"]')?.focus()
      })
    }
    hadOverlay.current = overlayOpen
  }, [confirm, routeEditor])

  return <>
    <AppShell
      contentPadding={0}
      sideNav={<SideNav aria-label="主导航"
        header={<SideNavHeading heading="LanDNS" icon={<img src="/logo.svg" alt="" width={32} height={32} />} />}
        footer={<SideNavSection title="应用" isHeaderHidden>
          <SideNavItem label="设置" icon={Settings2} isSelected={page === 'settings'} onClick={() => setPage('settings')} />
        </SideNavSection>}>
        <SideNavSection title="导航" isHeaderHidden>
          {pages.map(({ id, label, icon }) => <SideNavItem key={id} label={label} icon={icon} isSelected={page === id} onClick={() => setPage(id)} endContent={id === 'proxy' && config.routes.length > 0 ? <Badge label={config.routes.length} /> : undefined} />)}
        </SideNavSection>
      </SideNav>}>
      <Layout
        height="fill"
        header={<LayoutHeader><Stack direction="horizontal" gap={3} align="center" justify="between" wrap="wrap">
          <Heading level={1}>{page === 'overview' ? '我的项目' : currentPage.label}</Heading>
          <Stack direction="horizontal" gap={3} align="center" wrap="wrap">
            <Badge variant={running ? 'success' : 'neutral'} label={running ? '服务运行中' : '服务未启动'} />
            <Button size="md" label={serviceActive ? '停止服务' : '启动服务'} onClick={() => { void (serviceActive ? stop() : start()) }} isDisabled={!isDesktop || !loaded || locked || Boolean(connectionError)} isLoading={busy === '正在启动服务' || busy === '正在停止服务'} />
            {(page === 'overview' || page === 'proxy') && <Button size="md" label="添加本地项目" variant="primary" icon={<Icon icon={Plus} />} onClick={() => beginProject()} isDisabled={locked || !loaded} />}
            {page === 'logs' && <Button size="md" label="刷新" icon={<Icon icon={RefreshCw} />} isDisabled={!isDesktop || locked} isLoading={busy === '正在刷新日志'} onClick={() => { void action('正在刷新日志', refresh) }} />}
          </Stack>
        </Stack></LayoutHeader>}
        footer={dirty && !busy && <LayoutFooter><Stack direction="horizontal" gap={3} align="center" justify="between" wrap="wrap">
          <Text role="status">有未保存的更改</Text>
          <ButtonGroup label="保存更改"><Button label="放弃更改" onClick={discard} /><Button label={running ? '保存并应用' : '保存配置'} variant="primary" isDisabled={!isDesktop || Boolean(connectionError)} onClick={() => { void save() }} /></ButtonGroup>
        </Stack></LayoutFooter>}
        content={<LayoutContent padding={0}>
          <Stack padding={4} gap={6}>
            {!isDesktop && <Banner status="info" title="界面预览，实际使用请打开 LanDNS 桌面应用。" />}
            {connectionError && <Banner status="error" title="暂时无法连接桌面服务" description={connectionError} endContent={<Button label="重试" onClick={() => { void refresh() }} isDisabled={locked} />} />}
            {snapshot?.service.lastError && <Banner status="error" title={snapshot.service.lastError} />}

            {page === 'overview' && (project ? <Card><Stack gap={4}>
                <Stack direction="horizontal" gap={3} align="center" justify="between" wrap="wrap">
                  {currentConfig.routes.length > 1 ? <Selector label="选择项目" value={project.id} onChange={setSelectedProjectId} options={currentConfig.routes.map(route => ({ value: route.id, label: `${route.name}${route.enabled ? '' : '（已停用）'}` }))} /> : <Heading level={2}>{project.name}</Heading>}
                  <Badge variant={projectEnabled ? 'success' : 'neutral'} label={projectEnabled ? '已启用' : '未启用'} />
                </Stack>
                <Text wordBreak="break-word">{projectUrl}</Text>
                <Stack direction="horizontal" justify="end"><Button label="打开网址" variant="primary" icon={<Icon icon={ExternalLink} />} isDisabled={!isDesktop || !projectEnabled || locked} onClick={() => { void open(projectUrl) }} /></Stack>
                {!projectEnabled && <Banner status="info" title={!running ? '服务未启动' : !currentConfig.proxyEnabled ? '项目转发未启用' : '项目已停用'} endContent={<Button label="启用访问" isDisabled={!isDesktop || !loaded || locked || dirty || Boolean(connectionError)} onClick={() => { void enableProject() }} />} />}
              </Stack></Card> : <EmptyState icon={<Icon icon={Laptop} />} title="还没有项目" description="添加本地网址，例如 http://localhost:3002。" />)}

            {page === 'proxy' && <Stack gap={6}>
              {!currentConfig.proxyEnabled && currentConfig.routes.length > 0 && <Banner status="info" title="项目网址尚未启用。" endContent={<Button label="启用项目访问" isDisabled={!isDesktop || !loaded || locked || dirty || Boolean(connectionError)} onClick={() => { void enableProject() }} />} />}
              {config.routes.length === 0 ? <EmptyState icon={<Icon icon={Laptop} />} title="还没有项目" description="填写本地网址和访问网址即可添加。" /> : <Stack gap={4}>
                <TextInput label="搜索项目" isLabelHidden type="search" startIcon={Search} placeholder="搜索项目或网址…" value={filter} onChange={setFilter} />
                <Table aria-label="本地项目">
                  <TableHeader><TableRow><TableHeaderCell scope="col">项目</TableHeaderCell><TableHeaderCell scope="col">访问网址</TableHeaderCell><TableHeaderCell scope="col">本地网址</TableHeaderCell><TableHeaderCell scope="col">共享</TableHeaderCell><TableHeaderCell scope="col">操作</TableHeaderCell></TableRow></TableHeader>
                  <TableBody>{filteredRoutes.length === 0 ? <TableRow><TableCell colSpan={5}><EmptyState title="没有找到匹配的项目" /></TableCell></TableRow> : filteredRoutes.map(route => <TableRow key={route.id}>
                    <TableCell><Button label={route.name || (route.subdomain === '*' ? '默认项目' : route.subdomain)} variant="ghost" onClick={() => beginProject(route)} isDisabled={locked} /></TableCell>
                    <TableCell><Text wordBreak="break-word">{getProjectUrl(route, config)}</Text></TableCell>
                    <TableCell><Text wordBreak="break-word">http://localhost:{route.port}</Text></TableCell>
                    <TableCell><Switch label={`共享 ${route.name || route.subdomain}（保存后生效）`} isLabelHidden value={route.enabled} onChange={value => updateRoute(route.id, value)} isDisabled={locked} /></TableCell>
                    <TableCell><ButtonGroup label={`${route.name || route.subdomain} 的操作`}>
                      <IconButton label={`复制 ${route.name || route.subdomain} 的网址`} icon={<Icon icon={Copy} />} onClick={() => { void copy(getProjectUrl(route, config)) }} />
                      <IconButton label={`打开 ${route.name || route.subdomain}`} icon={<Icon icon={ExternalLink} />} isDisabled={!isDesktop || !running || !config.proxyEnabled || !route.enabled || dirty || locked} onClick={() => { void open(getProjectUrl(route, config)) }} />
                      <IconButton label={`移除 ${route.name || route.subdomain}`} icon={<Icon icon={Trash2} />} isDisabled={locked} onClick={() => setConfirm({ kind: 'route', route })} />
                    </ButtonGroup></TableCell>
                  </TableRow>)}</TableBody>
                </Table>
              </Stack>}
            </Stack>}

            {page === 'ngrok' && <NgrokPage desktop={isDesktop} disabled={!isDesktop || !loaded || locked || Boolean(connectionError)} busy={busy} routes={config.routes} run={action} />}

            {page === 'devices' && <Stack gap={6}>
              <Text>其他设备连接同一网络后，完成一次 DNS 设置。</Text>
              <Stack direction="horizontal" gap={3} justify="between" align="center" wrap="wrap">
                <MetadataList><MetadataListItem label="DNS 服务器">{network?.ip ?? '等待局域网 IP'}</MetadataListItem></MetadataList>
                <Button label="复制地址" icon={<Icon icon={Copy} />} isDisabled={!network} onClick={() => { if (network) void copy(network.ip) }} />
              </Stack>
              <Stack gap={3} align="start">
                <Heading level={2}>1. 连接同一个 Wi-Fi</Heading>
                <Text>保持 Mac 开机，让本地项目和 LanDNS 服务继续运行。</Text>
                {!serviceActive && <Button label="启动服务" isDisabled={!isDesktop || locked || !loaded || Boolean(connectionError)} onClick={() => { void start() }} />}
              </Stack>
              <Stack gap={3}>
                <Heading level={2}>2. 填写 DNS 地址</Heading>
                <Text>在另一台设备的 Wi-Fi 详情中，将「DNS 服务器」改为手动，填入上方地址。修改前记下原设置，停止使用时恢复为自动获取。</Text>
                <Text>只填写这台 Mac 的地址；混用公共 DNS 可能导致项目域名无法解析。</Text>
                <Collapsible trigger="让整个局域网自动接入" defaultIsOpen={false}><Text>在路由器的局域网 / DHCP 设置中，将下发的 DNS 服务器设为上方地址，再让设备重新连接 Wi-Fi。建议为 Mac 设置 DHCP 地址保留。</Text></Collapsible>
              </Stack>
              <Stack gap={3} align="start">
                <Heading level={2}>3. 打开项目网址</Heading>
                {project ? <Stack direction="horizontal" width="100%" gap={3} align="center" justify="between">
                  <Text wordBreak="break-word">{projectUrl}</Text>
                  <ButtonGroup label="项目网址"><Button label="复制网址" icon={<Icon icon={Copy} />} onClick={() => { void copy(projectUrl) }} /><Button label="查看其他项目" onClick={() => setPage('proxy')} /></ButtonGroup>
                </Stack> : <Button label="添加本地项目" icon={<Icon icon={Plus} />} isDisabled={locked || !loaded} onClick={() => beginProject()} />}
              </Stack>
              <Collapsible trigger="还是打不开？" defaultIsOpen={false}><Stack gap={3}>
                <Text>先确认 Mac 上的 localhost 网址可以打开，再在概览中点击「打开网址」。</Text>
                <Text>LanDNS 会自动同步本机系统代理绕过列表。代理软件重写设置后，可保存并应用配置或重新启动服务，再次同步。</Text>
                <Text>Mac 的 IP 变化后，域名会自动更新；其他设备填写的 DNS 地址也要同步更新。</Text>
                <Text>允许 macOS 防火墙中的 LanDNS 入站连接。访客 Wi-Fi、设备隔离、VPN 或浏览器安全 DNS 可能影响接入。Mac 睡眠、关机或断网时，以它作为唯一 DNS 的设备也可能无法打开普通网站。</Text>
                <Text>项目提示域名不被允许时，在项目的主机名白名单中加入该域名，例如 Vite 的 server.allowedHosts。</Text>
              </Stack></Collapsible>
            </Stack>}

            {page === 'logs' && <Stack gap={4}>
              <Text>{snapshot?.logs.length ?? 0} 条记录 · 每 4 秒自动刷新</Text>
              {!snapshot?.logs.length ? <EmptyState icon={<Icon icon={TerminalSquare} />} title="暂无运行记录" description="启动服务后显示运行日志。" /> : <div role="log" aria-label="服务日志"><Table aria-label="服务日志"><TableHeader><TableRow><TableHeaderCell scope="col">时间</TableHeaderCell><TableHeaderCell scope="col">级别</TableHeaderCell><TableHeaderCell scope="col">记录</TableHeaderCell></TableRow></TableHeader><TableBody>{[...snapshot.logs].reverse().map((entry, index) => <TableRow key={`${entry.timestamp}-${index}`}><TableCell><time dateTime={new Date(entry.timestamp * 1000).toISOString()}>{displayTime(entry.timestamp)}</time></TableCell><TableCell><Badge variant={entry.level.toLowerCase() === 'error' ? 'error' : entry.level.toLowerCase() === 'warn' ? 'warning' : 'neutral'} label={entry.level.toUpperCase()} /></TableCell><TableCell><Text wordBreak="break-word">{entry.message}</Text></TableCell></TableRow>)}</TableBody></Table></div>}
            </Stack>}

            {page === 'settings' && <Stack gap={6}>
              <FormLayout>
                <Selector id="theme-mode" label="主题" value={themeMode} onChange={changeTheme} options={[{ value: 'system', label: '跟随系统' }, { value: 'light', label: '浅色' }, { value: 'dark', label: '深色' }]} />
              </FormLayout>
              <Stack gap={3}>
                <Heading level={2}>应用更新</Heading>
                <Text>当前版本：{appVersion ? `v${appVersion}` : isDesktop ? '读取中…' : '桌面应用中可用'}</Text>
                {isDesktop && appVersion && !updaterAvailable && <Text>请使用正式打包的 LanDNS 应用检查更新。</Text>}
                <Text role="status">{busy === '正在更新应用' ? '' : updateStatus}</Text>
                {busy === '正在更新应用' && <ProgressBar label={updateStatus} value={updateProgress ?? undefined} isIndeterminate={updateProgress === null} hasValueLabel />}
                {updateError && <Banner status="error" title={updateError} role="alert" />}
                {availableUpdate?.body && <Collapsible trigger="更新说明" defaultIsOpen={false}><Text style={{ whiteSpace: 'pre-wrap' }}>{availableUpdate.body}</Text></Collapsible>}
                {availableUpdate && <Text>安装后自动重启应用；运行中的共享服务会同步更新，macOS 可能请求管理员授权。</Text>}
                {availableUpdate && dirty && <Text>请先保存或放弃尚未保存的更改，再安装更新。</Text>}
                <Stack direction="horizontal" gap={3} wrap="wrap">
                  <Button label="检查更新" icon={<Icon icon={RefreshCw} />} isDisabled={!updaterAvailable || locked || updateInstalled} isLoading={busy === '正在检查更新'} onClick={() => { void checkForUpdates() }} />
                  {availableUpdate && <Button label={updateInstalled ? '完成更新并重启' : `下载安装 v${availableUpdate.version}`} variant="primary" isDisabled={!updaterAvailable || locked || dirty || !loaded || Boolean(connectionError)} isLoading={busy === '正在更新应用'} onClick={() => { void installUpdate() }} />}
                </Stack>
              </Stack>
              <Text>网络设置通常保持默认即可。</Text>
              <Collapsible trigger="高级设置" isOpen={advancedOpen} onOpenChange={setAdvancedOpen}><Stack gap={6}>
                <Stack gap={3}>
                  <Heading level={2} id="diagnostic-title">DNS 检测</Heading>
                  <Text>检查域名是否指向这台 Mac，不检测网页。</Text>
                </Stack>
                <form aria-labelledby="diagnostic-title" onSubmit={event => { event.preventDefault(); if (!isDesktop || !running || locked) return; if (!hostname.trim()) { document.getElementById('diagnostic-hostname')?.focus(); toast({ body: '请填写要检测的域名。', type: 'error' }); return } void testDns(hostname) }}><FormLayout>
                  <TextInput id="diagnostic-hostname" htmlName="hostname" label="检测域名 *" autoCapitalize="none" hasAutoCorrect={false} hasSpellCheck={false} value={hostname} onChange={setHostname} placeholder={activeDomain} isRequired isDisabled={locked} />
                  <Button label="检测" type="submit" isDisabled={!isDesktop || !running || locked} isLoading={busy === '正在检查解析'} />
                </FormLayout></form>
                {diagnostic ? <Banner status={diagnostic.ok ? 'success' : 'error'} title={diagnostic.ok ? 'DNS 解析正常' : 'DNS 解析未通过'} description={`${diagnostic.message}${diagnostic.resolvedIp ? ` · ${diagnostic.resolvedIp}` : ''} · ${diagnostic.elapsedMs} ms`} role="status" /> : !running && <Text>启动服务后可检测</Text>}
                <MetadataList title="网络状态">
                  <MetadataListItem label="Mac 局域网地址"><Stack direction="horizontal" gap={3} align="center">{network?.ip ?? '未检测到'}{network && <IconButton label="复制局域网地址" icon={<Icon icon={Copy} />} onClick={() => { void copy(network.ip) }} />}</Stack></MetadataListItem>
                  <MetadataListItem label="网络接口">{network?.interfaceName ?? '—'} · {currentConfig.interfaceName ? '手动选择' : '自动选择'}</MetadataListItem>
                  <MetadataListItem label="DNS 解析">{running ? '已启动' : '未启动'} · :53</MetadataListItem>
                  <MetadataListItem label="项目转发">{currentConfig.proxyEnabled ? `${enabledRoutes} 个启用项目` : '未启用'} · :80</MetadataListItem>
                  <MetadataListItem label="已处理请求">{snapshot ? number.format(snapshot.service.dnsQueries) : '—'} DNS · {snapshot ? number.format(snapshot.service.proxyRequests) : '—'} HTTP</MetadataListItem>
                </MetadataList>
                <form id="settings-form" aria-label="DNS 与网络配置" onSubmit={event => { event.preventDefault(); void save() }}><FormLayout>
                  <TextInput id="domain" htmlName="domain" label="网址后缀 *" description="可自定义，例如 dev.test；默认 dev.home.arpa。" isRequired hasAutoCorrect={false} autoCapitalize="none" hasSpellCheck={false} value={config.domain} onChange={value => change({ domain: value.toLowerCase() })} placeholder="dev.home.arpa" isDisabled={locked} status={settingsError?.field === 'domain' ? { type: 'error', message: settingsError.message } : undefined} />
                  <Switch id="proxy-enabled" label="项目转发" description="添加项目时自动开启；关闭后项目网址无法访问。" value={config.proxyEnabled} onChange={value => change({ proxyEnabled: value })} isDisabled={locked} />
                  <TextInput id="upstream" htmlName="upstreamDns" label="普通网站使用的 DNS *" description="用于查询普通网站的地址。" isRequired hasAutoCorrect={false} autoCapitalize="none" inputMode="decimal" hasSpellCheck={false} value={config.upstreamDns} onChange={value => change({ upstreamDns: value })} placeholder="1.1.1.1" isDisabled={locked} status={settingsError?.field === 'upstream' ? { type: 'error', message: settingsError.message } : undefined} />
                  <NumberInput id="ttl" htmlName="ttl" label="地址缓存时间 *" description="DNS 结果的缓存时间，默认 15 秒。" units="秒" isRequired hasClear min={5} max={300} isIntegerOnly value={config.ttl || null} onChange={value => change({ ttl: value ?? 0 })} onEntryErrorChange={error => { ttlEntryError.current = error?.kind ?? null }} isDisabled={locked} status={settingsError?.field === 'ttl' ? { type: 'error', message: settingsError.message } : undefined} />
                  <Selector id="network-interface" htmlName="interfaceName" label="网络连接" description="默认自动选择当前局域网。" value={config.interfaceName} onChange={value => change({ interfaceName: value })} isDisabled={locked} options={[{ value: '', label: '自动选择' }, ...(snapshot?.interfaces.map(iface => ({ value: iface.interfaceName, label: `${iface.interfaceName} · ${iface.ip}` })) ?? []), ...(config.interfaceName && !snapshot?.interfaces.some(iface => iface.interfaceName === config.interfaceName) ? [{ value: config.interfaceName, label: `${config.interfaceName}（当前不可用）` }] : [])]} />
                  <Text>Mac 地址变化后，每 3 秒自动检查并更新。</Text>
                </FormLayout></form>
              </Stack></Collapsible>
              <Stack gap={3} align="start">
                <Heading level={2}>共享服务</Heading>
                <Text>{snapshot?.installed ? '关闭窗口后服务继续运行。停止服务也会暂停开机自动运行。' : '首次启用项目需要 macOS 管理员授权。'}</Text>
                <Badge label={snapshot?.installed ? '已安装' : '未安装'} />
                {serviceActive && appVersion && snapshot?.service.version !== appVersion && !updateInstalled && <Banner status="info" title="共享服务需要更新" description="同步到当前应用版本，项目网址会短暂中断；macOS 可能请求管理员授权。" endContent={<Button label="更新共享服务" isDisabled={locked || dirty || Boolean(connectionError)} onClick={() => { void action('正在更新共享服务', () => api.startService(currentConfig), '共享服务已更新。') }} />} />}
              </Stack>
              <Stack gap={3} align="start">
                <Heading level={3}>卸载共享服务</Heading>
                <Text>停止项目网址访问并移除本机 DNS 配置，保留项目列表。</Text>
                <Button label="卸载服务" variant="destructive" isDisabled={!isDesktop || !snapshot?.installed || locked} onClick={() => setConfirm({ kind: 'uninstall' })} />
              </Stack>
            </Stack>}
          </Stack>
        </LayoutContent>} />
    </AppShell>
    <Dialog isOpen={Boolean(routeEditor)} purpose={locked ? 'required' : 'form'} onOpenChange={value => { if (!value && !locked) setRouteEditor(null) }}>
      {routeEditor && <Layout
        header={<DialogHeader title={config.routes.some(route => route.id === routeEditor.id) ? '编辑本地项目' : '添加本地项目'} onOpenChange={locked ? undefined : value => { if (!value) setRouteEditor(null) }} />}
        content={<LayoutContent>
          <form id="project-form" aria-label="本地项目" onSubmit={submitRoute}>
            <FormLayout>
              {settingsError && <Banner status="error" title={settingsError.message} description="请关闭弹窗，在设置 → 高级设置中修改。" />}
              <TextInput id="route-address" htmlName="localAddress" label="本地网址 *" inputMode="url" hasAutoFocus isRequired hasAutoCorrect={false} autoCapitalize="none" hasSpellCheck={false} placeholder="http://localhost:3002" value={localAddress} isDisabled={locked} status={routeError?.field === 'address' ? { type: 'error', message: routeError.message } : undefined} onChange={value => { setRouteError(null); setLocalAddress(value) }} />
              <TextInput id="route-url" htmlName="projectAddress" label="访问网址 *" description={`填写完整网址可单独设置域名；只填 sub 则使用 sub.${config.domain}。`} inputMode="url" isRequired hasAutoCorrect={false} autoCapitalize="none" hasSpellCheck={false} placeholder="http://sub.dev.test" value={projectAddress} isDisabled={locked} status={routeError?.field === 'url' ? { type: 'error', message: routeError.message } : undefined} onChange={value => { setRouteError(null); projectAddressEdited.current = true; setProjectAddress(value) }} />
              <TextInput id="route-name" htmlName="name" label="项目备注" placeholder="例如：我的网站" value={routeEditor.name} isDisabled={locked} status={routeError?.field === 'name' ? { type: 'error', message: routeError.message } : undefined} onChange={value => { setRouteError(null); setRouteEditor({ ...routeEditor, name: value }) }} />
            </FormLayout>
          </form>
        </LayoutContent>}
        footer={<LayoutFooter><Stack gap={3}>
          <Text>{dirty ? '同时应用其他尚未保存的更改；macOS 可能请求管理员授权。' : '启用时需要 macOS 管理员授权。'}</Text>
          <Stack direction="horizontal" justify="end" gap={3}>
            <Button label="取消" isDisabled={locked} onClick={() => setRouteEditor(null)} />
            <Button label="保存并启用" variant="primary" type="submit" form="project-form" isDisabled={!isDesktop || !loaded || locked || Boolean(connectionError)} isLoading={busy === '正在保存并启用项目'} />
          </Stack>
        </Stack></LayoutFooter>} />}
    </Dialog>
    <AlertDialog isOpen={Boolean(confirm)} onOpenChange={value => { if (!value && !locked) setConfirm(null) }} title={confirm?.kind === 'uninstall' ? '卸载 LanDNS 后台服务？' : '移除这个本地项目？'} description={confirm?.kind === 'uninstall' ? '这会停止 DNS 和 HTTP 代理，移除 Mac 本机的域名解析配置及 LanDNS 新增的代理绕过规则。其他设备若使用这台 Mac 作为 DNS，需要恢复其网络设置。应用内的设置会保留。' : `“${confirm?.kind === 'route' ? confirm.route.name || confirm.route.subdomain : ''}”将从配置中移除，保存并应用后生效。`} cancelLabel="取消" actionLabel={confirm?.kind === 'uninstall' ? '卸载服务' : '移除项目'} isActionLoading={locked} onAction={() => {
      if (locked) return
      if (confirm?.kind === 'route') { change({ routes: config.routes.filter(route => route.id !== confirm.route.id) }); setConfirm(null) }
      else if (confirm?.kind === 'uninstall') { void action('正在卸载服务', async () => { await api.uninstallService(); setConfirm(null) }, '后台服务已卸载。请同步恢复其他设备的 DNS 设置。') }
    }} />
  </>
}
