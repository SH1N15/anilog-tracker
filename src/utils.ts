import type { Anime, AnimeTitle, Season, UiLanguage } from './types';
import { tr } from './i18n';

export const SEASONS: Array<{ value: Season; label: string; months: string }> = [
  { value: 'WINTER', label: '冬', months: '1–3 月' },
  { value: 'SPRING', label: '春', months: '4–6 月' },
  { value: 'SUMMER', label: '夏', months: '7–9 月' },
  { value: 'FALL', label: '秋', months: '10–12 月' },
];

export function currentSeason(date = new Date()): { season: Season; year: number } {
  const month = date.getMonth() + 1;
  const season: Season = month <= 3 ? 'WINTER' : month <= 6 ? 'SPRING' : month <= 9 ? 'SUMMER' : 'FALL';
  return { season, year: date.getFullYear() };
}

export function titleOf(title?: AnimeTitle | null, language: UiLanguage = 'zh-CN'): string {
  return title?.native || title?.english || title?.romaji || tr(language, '未命名番剧', 'Untitled anime');
}

export function reminderTitleOf(title?: AnimeTitle | null, language: UiLanguage = 'zh-CN'): string {
  return title?.english || title?.romaji || title?.native || tr(language, '未命名番剧', 'Untitled anime');
}

export function secondaryTitle(title?: AnimeTitle | null, language: UiLanguage = 'zh-CN'): string {
  const primary = titleOf(title, language);
  return [title?.english, title?.romaji].find((value) => value && value !== primary) || '';
}

export function formatLabel(format?: string | null, language: UiLanguage = 'zh-CN'): string {
  const labels: Record<string, [string, string]> = {
    TV: ['TV', 'TV'],
    TV_SHORT: ['短篇', 'TV Short'],
    MOVIE: ['电影', 'Movie'],
    SPECIAL: ['特别篇', 'Special'],
    OVA: ['OVA', 'OVA'],
    ONA: ['网络动画', 'ONA'],
    MUSIC: ['音乐', 'Music'],
  };
  return format ? (labels[format] ? tr(language, ...labels[format]) : format) : tr(language, '待定', 'TBA');
}

export function seasonName(season: Season, language: UiLanguage = 'zh-CN'): string {
  const names: Record<Season, [string, string]> = {
    WINTER: ['冬', 'Winter'], SPRING: ['春', 'Spring'], SUMMER: ['夏', 'Summer'], FALL: ['秋', 'Fall'],
  };
  return tr(language, ...names[season]);
}

export function seasonMonths(season: Season, language: UiLanguage = 'zh-CN'): string {
  const months: Record<Season, [string, string]> = {
    WINTER: ['1–3 月', 'Jan–Mar'], SPRING: ['4–6 月', 'Apr–Jun'], SUMMER: ['7–9 月', 'Jul–Sep'], FALL: ['10–12 月', 'Oct–Dec'],
  };
  return tr(language, ...months[season]);
}

export function seasonLabel(season: Season, year: number, language: UiLanguage = 'zh-CN'): string {
  return language === 'en-US' ? `${seasonName(season, language)} ${year}` : `${year} ${seasonName(season, language)}季`;
}

export function localAiringWeekday(anime: Anime, now = Math.floor(Date.now() / 1000)): number {
  const fallbackAt = anime.nextAiringEpisode?.airingAt;
  const scheduledAt = (anime.airingSchedule?.nodes || [])
    .map((node) => node.airingAt)
    .filter((airingAt) => Number.isFinite(airingAt) && airingAt > now)
    .reduce<number | null>((earliest, airingAt) => earliest === null || airingAt < earliest ? airingAt : earliest, null);
  const validFallbackAt = Number.isFinite(fallbackAt) && fallbackAt! > now ? fallbackAt! : null;
  const airingAt = scheduledAt === null
    ? validFallbackAt
    : validFallbackAt === null
      ? scheduledAt
      : Math.min(scheduledAt, validFallbackAt);
  if (airingAt === null) return 7;
  const day = new Date(airingAt * 1000).getDay();
  return day === 0 ? 6 : day - 1;
}

export function formatAiring(timestamp?: number | null, includeDate = true, language: UiLanguage = 'zh-CN', precision?: string): string {
  if (!timestamp) return tr(language, '播出时间待定', 'Airing time TBA');
  if (precision === 'date' || precision === 'unknown') {
    return new Intl.DateTimeFormat(language, { month: 'numeric', day: 'numeric', weekday: 'short', timeZone: 'UTC' })
      .format(new Date(timestamp * 1000));
  }
  return new Intl.DateTimeFormat(language, {
    ...(includeDate ? { month: 'numeric', day: 'numeric', weekday: 'short' } : {}),
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(new Date(timestamp * 1000));
}

export function relativeTime(timestamp?: number | null, language: UiLanguage = 'zh-CN', precision?: string): string {
  if (!timestamp) return tr(language, '尚未公布', 'Not announced');
  if (precision === 'date' || precision === 'unknown') return tr(language, '时刻待定', 'Time TBA');
  const seconds = timestamp - Math.floor(Date.now() / 1000);
  if (seconds <= 0) return tr(language, '已播出', 'Aired');
  const days = Math.floor(seconds / 86400);
  if (days > 0) return tr(language, `${days} 天后`, `in ${days} day${days === 1 ? '' : 's'}`);
  const hours = Math.floor(seconds / 3600);
  if (hours > 0) return tr(language, `${hours} 小时后`, `in ${hours} hour${hours === 1 ? '' : 's'}`);
  const minutes = Math.max(1, Math.floor(seconds / 60));
  return tr(language, `${minutes} 分钟后`, `in ${minutes} minute${minutes === 1 ? '' : 's'}`);
}

export function stripDescription(value?: string | null, language: UiLanguage = 'zh-CN'): string {
  if (!value) return tr(language, '暂无简介', 'No description available');
  return value.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim();
}
