import type { UiLanguage } from './types';
import { IS_ORIGINAL_EDITION } from './edition';

export function normalizeUiLanguage(value?: string | null): UiLanguage {
  if (!IS_ORIGINAL_EDITION) return 'zh-CN';
  return value?.toLowerCase().startsWith('zh') ? 'zh-CN' : value?.toLowerCase().startsWith('en') ? 'en-US' : detectUiLanguage();
}

export function detectUiLanguage(): UiLanguage {
  if (!IS_ORIGINAL_EDITION) return 'zh-CN';
  const language = typeof navigator === 'undefined' ? '' : navigator.language;
  return language.toLowerCase().startsWith('zh') ? 'zh-CN' : 'en-US';
}

export function tr(language: UiLanguage, chinese: string, english: string): string {
  return language === 'en-US' ? english : chinese;
}

const BACKEND_MESSAGES: Array<[RegExp, string]> = [
  [/当前平台不支持 WebDAV 同步/g, 'WebDAV sync is not supported on this platform'],
  [/请先启用 WebDAV 同步/g, 'Enable WebDAV sync first'],
  [/WebDAV 同步文件不是有效的 JSON/g, 'The WebDAV sync file is not valid JSON'],
  [/WebDAV 文件在同步期间反复变化，请稍后重试/g, 'The WebDAV file kept changing during sync. Try again later'],
  [/已合并(?:电脑端|另一台设备)的更新/g, 'Updates from the other device were merged'],
  [/两端数据已同步/g, 'Both devices are in sync'],
  [/WebDAV 同步失败/g, 'WebDAV sync failed'],
  [/请输入有效的 WebDAV HTTPS 地址/g, 'Enter a valid WebDAV HTTPS address'],
  [/WebDAV 地址必须是无账号、参数或片段的 HTTPS 地址/g, 'The WebDAV address must be an HTTPS URL without credentials, query parameters, or fragments'],
  [/Windows 安全存储当前不可用/g, 'Windows secure storage is currently unavailable'],
  [/WebDAV 密码无法解密，请重新输入密码/g, 'The WebDAV password could not be decrypted. Enter it again'],
  [/请先完整填写 WebDAV 地址、用户名和密码/g, 'Enter the WebDAV address, username, and password first'],
  [/启用同步前请完整填写地址、用户名和密码/g, 'Enter the address, username, and password before enabling sync'],
  [/应用正在退出/g, 'The app is shutting down'],
  [/WebDAV 认证失败，请检查账号和应用密码/g, 'WebDAV authentication failed. Check the username and app password'],
  [/无法创建 AniLog 同步目录/g, 'Could not create the AniLog sync folder'],
  [/读取 WebDAV 同步文件失败/g, 'Could not read the WebDAV sync file'],
  [/写入 WebDAV 同步文件失败/g, 'Could not write the WebDAV sync file'],
  [/WebDAV 同步文件为空/g, 'The WebDAV sync file is empty'],
  [/WebDAV 同步文件超过 5 MB，已停止读取/g, 'The WebDAV sync file exceeds 5 MB and was not read'],
  [/WebDAV 连接失败/g, 'WebDAV connection failed'],
  [/WebDAV 连接成功/g, 'WebDAV connection succeeded'],
  [/AniList 同步失败/g, 'AniList sync failed'],
  [/AniList 暂时不可用/g, 'AniList is temporarily unavailable'],
  [/AniList 返回了无效数据/g, 'AniList returned invalid data'],
  [/无法读取本地状态/g, 'Could not read local data'],
  [/无法读取本季番剧/g, 'Could not load this season'],
  [/无法读取缓存大小/g, 'Could not calculate cache size'],
  [/无法读取 WebDAV 设置/g, 'Could not read WebDAV settings'],
  [/Original 版不支持 Bangumi/g, 'Bangumi is not supported in the Original edition'],
  [/当前平台不支持 Bangumi Token 存储/g, 'Bangumi token storage is not supported on this platform'],
  [/Token 不能为空/g, 'Token cannot be empty'],
  [/尚未保存 Bangumi Token/g, 'No Bangumi token saved yet'],
  [/Bangumi 授权失败，Token 可能已失效/g, 'Bangumi authorization failed; the token may have expired'],
  [/无法连接 Bangumi 服务（网络连接被重置或拦截，请检查当前网络、VPN 或反代域名）/g, 'Cannot reach the Bangumi service (the network connection was reset or blocked; check the current network, VPN, or proxy domain)'],
  [/Bangumi 服务连接超时（请检查当前网络、VPN 或反代域名）/g, 'The Bangumi service timed out (check the current network, VPN, or proxy domain)'],
  [/Bangumi 反代返回 HTTP 502（反代暂时无法连接上游 Bangumi）/g, 'The Bangumi proxy returned HTTP 502 (it cannot currently reach the upstream Bangumi service)'],
  [/Bangumi 反代返回 HTTP (\d+)/g, 'The Bangumi proxy returned HTTP $1'],
  [/Bangumi 安全存储不可用/g, 'Bangumi secure storage is unavailable'],
  [/当前平台不支持 Bangumi 账户同步/g, 'Bangumi account sync is not supported on this platform'],
];

export function localizeMessage(message: string, language: UiLanguage): string {
  if (language !== 'en-US') return message;
  return BACKEND_MESSAGES.reduce((result, [pattern, replacement]) => result.replace(pattern, replacement), message);
}
