import { invoke, isTauri } from '@tauri-apps/api/core';
import type { Config, DiagnosticResult, Snapshot } from './types';

export const isDesktop = isTauri();

function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isDesktop) return Promise.reject(new Error('请在 LanDNS 桌面应用中使用服务功能。'));
  return invoke<T>(command, args);
}

export const api = {
  snapshot: () => call<Snapshot>('get_snapshot'),
  saveConfig: (config: Config) => call<void>('save_config', { config }),
  startService: (config: Config) => call<void>('start_service', { config }),
  stopService: () => call<void>('stop_service'),
  uninstallService: () => call<void>('uninstall_service'),
  diagnose: (hostname: string) => call<DiagnosticResult>('diagnose', { hostname }),
  openUrl: (url: string) => call<void>('open_url', { url }),
};
