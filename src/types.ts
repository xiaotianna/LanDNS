export interface ProxyRoute {
  id: string;
  name: string;
  subdomain: string;
  port: number;
  enabled: boolean;
}

export interface Config {
  domain: string;
  upstreamDns: string;
  interfaceName: string;
  proxyEnabled: boolean;
  ttl: number;
  routes: ProxyRoute[];
}

export interface NetworkInfo { interfaceName: string; ip: string; netmask: string }
export interface ServiceStatus {
  running: boolean;
  version: string | null;
  pid: number | null;
  ip: string | null;
  interfaceName: string | null;
  startedAt: number | null;
  updatedAt: number | null;
  dnsQueries: number;
  proxyRequests: number;
  lastError: string | null;
}
export interface LogEntry { timestamp: number; level: string; message: string }
export interface Snapshot {
  config: Config;
  network: NetworkInfo | null;
  interfaces: NetworkInfo[];
  service: ServiceStatus;
  logs: LogEntry[];
  installed: boolean;
  serviceLoaded: boolean;
}
export interface DiagnosticResult {
  hostname: string;
  expectedIp: string | null;
  resolvedIp: string | null;
  elapsedMs: number;
  ok: boolean;
  message: string;
}
export const DEFAULT_CONFIG: Config = {
  domain: 'dev.home.arpa', upstreamDns: '1.1.1.1', interfaceName: '',
  proxyEnabled: false, ttl: 15, routes: [],
};
