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

export function parseProjectAddress(value: string, domain: string): Pick<ProxyRoute, 'subdomain' | 'hostname'> | { error: string } {
  const address = value.trim()
  if (!address) return { error: '请填写访问网址，例如 http://sub.dev.test。' }
  if (address === '*') return { subdomain: '*', hostname: null }
  if (/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/i.test(address)) {
    if (address.length + domain.length + 1 > 253) return { error: '完整域名不能超过 253 个字符，请缩短名称或默认后缀。' }
    return { subdomain: address.toLowerCase(), hostname: null }
  }
  if (/[\\\s\u0000-\u001f\u007f]/.test(address)) return { error: '网址中不能包含空格、换行或反斜杠。' }
  let url: URL
  try {
    url = new URL(address.includes('://') ? address : `http://${address}`)
  } catch {
    return { error: '网址格式不正确，请填写例如 http://sub.dev.test。' }
  }
  if (url.protocol !== 'http:' || url.port) return { error: '访问网址使用 HTTP 默认端口，请填写例如 http://sub.dev.test。' }
  if (url.username || url.password || url.pathname !== '/' || url.search || url.hash) {
    return { error: '请只填写域名网址，不要附带账号、页面路径、查询参数或锚点。' }
  }
  const hostname = url.hostname
  if (hostname.length > 253 || !hostname.split('.').every(label => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label)) || /^(\d+\.){3}\d+$/.test(hostname)) {
    return { error: '请填写有效域名，例如 sub.dev.test，不能使用 IP 地址。' }
  }
  return { hostname, subdomain: hostname.split('.')[0] }
}

export function getProjectUrl(route: ProxyRoute, config: Config): string {
  if (route.hostname) return `http://${route.hostname}`
  let name = route.subdomain
  if (name === '*') {
    name = 'project'
    let suffix = 2
    while (config.routes.some(candidate => candidate.enabled && (candidate.hostname ?? `${candidate.subdomain}.${config.domain}`) === `${name}.${config.domain}`)) {
      name = `project${suffix++}`
    }
  }
  return `http://${name}.${config.domain}`
}
