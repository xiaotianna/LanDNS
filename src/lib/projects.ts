import type { Config, ProxyRoute } from '../types'

export function parseLocalAddress(value: string): { port: number } | { error: string } {
  const address = value.trim()
  if (!address) return { error: '请填写项目的本地网址，例如 http://localhost:3002。' }
  let url: URL
  try {
    url = new URL(address.includes('://') ? address : `http://${address}`)
  } catch {
    return { error: '这个网址格式不正确，请填写例如 http://localhost:3002。' }
  }
  if (url.protocol !== 'http:') return { error: '目前支持 HTTP 项目，请填写以 http:// 开头的本地网址。' }
  if (!['localhost', '127.0.0.1'].includes(url.hostname) || url.username || url.password) {
    return { error: '请填写这台 Mac 上的项目网址，例如 http://localhost:3002 或 http://127.0.0.1:3002。' }
  }
  if (url.pathname !== '/' || url.search || url.hash) {
    return { error: '请只填写本地服务地址，例如 http://localhost:3002；页面路径可在打开项目后追加。' }
  }
  const port = Number(url.port || 80)
  if (port < 1 || port > 65535 || port === 53 || port === 80) {
    return { error: '请填写项目实际使用的端口，例如 3002。53 和 80 已用于 LanDNS 服务。' }
  }
  return { port }
}

export function getProjectUrl(route: ProxyRoute, config: Config): string {
  let name = route.subdomain
  if (name === '*') {
    name = 'project'
    let suffix = 2
    while (config.routes.some(candidate => candidate.enabled && candidate.subdomain === name)) {
      name = `project${suffix++}`
    }
  }
  return `http://${name}.${config.domain}`
}
