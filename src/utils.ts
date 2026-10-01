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
  const nodes = [...(anime.airingSchedule?.nodes || []), ...(anime.nextAiringEpisode ? [anime.nextAiringEpisode] : [])];
  const next = nodes.filter((node) => Number.isFinite(node.airingAt) && (
    node.airingPrecision === 'date' || node.airingPrecision === 'unknown'
      ? Math.floor(node.airingAt / 86400) >= Math.floor(now / 86400)
      : node.airingAt > now
  )).sort((a, b) => a.airingAt - b.airingAt)[0];
  if (next) {
    const date = new Date(next.airingAt * 1000);
    const day = next.airingPrecision === 'date' || next.airingPrecision === 'unknown' ? date.getUTCDay() : date.getDay();
    return day === 0 ? 6 : day - 1;
  }
  // Premiere dates and broadcaster weekdays belong to their source calendar.
  // Do not invent a midnight instant or shift them into the device time zone.
  if (Number.isInteger(anime.broadcastWeekday) && anime.broadcastWeekday! >= 1 && anime.broadcastWeekday! <= 7) {
    return anime.broadcastWeekday! - 1;
  }
  const premiere = premiereDate(anime);
  if (!premiere) return 7;
  const day = premiere.getUTCDay();
  return day === 0 ? 6 : day - 1;
}

function premiereDate(anime: Anime): Date | null {
  const { year, month, day } = anime.startDate || {};
  if (!Number.isInteger(year) || !Number.isInteger(month) || !Number.isInteger(day)) return null;
  const date = new Date(Date.UTC(year!, month! - 1, day!));
  return date.getUTCFullYear() === year && date.getUTCMonth() === month! - 1 && date.getUTCDate() === day ? date : null;
}

export function seasonDateLabel(anime: Anime, language: UiLanguage = 'zh-CN'): string {
  const premiere = premiereDate(anime);
  if (premiere) {
    const date = new Intl.DateTimeFormat(language, { month: 'numeric', day: 'numeric', weekday: 'short', timeZone: 'UTC' }).format(premiere);
    return language === 'en-US' ? `Premiere ${date} · Time TBA` : `首播 ${date} · 时刻待定`;
  }
  if (Number.isInteger(anime.broadcastWeekday) && anime.broadcastWeekday! >= 1 && anime.broadcastWeekday! <= 7) {
    const day = new Intl.DateTimeFormat(language, { weekday: 'long', timeZone: 'UTC' }).format(new Date(Date.UTC(2024, 0, anime.broadcastWeekday!)));
    return language === 'en-US' ? `${day} · Time TBA` : `${day} · 时刻待定`;
  }
  return tr(language, '播出日期待定', 'Release date TBA');
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
