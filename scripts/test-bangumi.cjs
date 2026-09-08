const assert = require('node:assert/strict');
const { bangumiSearchKeywords, matchBangumiCandidates, matchOfflineBangumi } = require('../electron/bangumi.cjs');

const smoking = {
  id: 196187,
  title: {
    native: 'スーパーの裏でヤニ吸うふたり',
    romaji: 'Super no Ura de Yani Suu Futari',
    english: 'Smoking Behind the Supermarket with You',
  },
  seasonYear: 2026,
  format: 'TV',
};

const offlineMatch = matchOfflineBangumi(smoking);
assert.equal(offlineMatch?.status, 'matched');
assert.match(offlineMatch.nameCn, /超市/);

const reZeroSeason4 = {
  id: 189046,
  title: {
    native: 'Re:ゼロから始める異世界生活 4th season',
    romaji: 'Re:Zero kara Hajimeru Isekai Seikatsu 4th Season',
    english: 'Re:ZERO -Starting Life in Another World- Season 4',
  },
  format: 'TV',
};
const aggregateMatch = matchOfflineBangumi(reZeroSeason4);
assert.equal(aggregateMatch?.status, 'matched');
assert.equal(aggregateMatch?.source, 'anilist-id-aggregate');
assert.equal(aggregateMatch?.nameCn, 'Re：从零开始的异世界生活 第四季');
assert.deepEqual(aggregateMatch?.subjectIds, [547888, 633836]);

const sequel = {
  id: 1,
  title: { native: '作品 第二期', english: 'Example Season 2' },
  seasonYear: 2026,
  format: 'TV',
};
const rankedMatch = matchBangumiCandidates(sequel, [
  { id: 10, type: 2, name: '作品 第二期', name_cn: '作品 第二季', date: '2026-07-01', platform: 'TV' },
  { id: 11, type: 2, name: '作品', name_cn: '作品', date: '2020-07-01', platform: 'TV' },
]);
assert.equal(rankedMatch.status, 'matched');
assert.equal(rankedMatch.subjectId, 10);

const aoashi = {
  id: 191788,
  title: { native: 'アオアシ 第2期', romaji: 'Aoashi 2nd Season' },
  format: 'TV',
  seasonYear: 2026,
  startDate: { year: 2026, month: 10, day: 4 },
};
const aoashiMatch = matchBangumiCandidates(aoashi, [
  { id: 337459, type: 2, name: 'アオアシ', name_cn: '青之芦苇', date: '2022-04-09', platform: 'TV' },
  { id: 555605, type: 2, name: 'アオアシ Season 2', name_cn: '青之芦苇 第二季', date: '2026-10-04', platform: 'TV' },
]);
assert.equal(aoashiMatch.status, 'matched');
assert.equal(aoashiMatch.subjectId, 555605);
assert.equal(aoashiMatch.nameCn, '青之芦苇 第二季');

const steelBallRun = {
  id: 210482,
  title: {
    native: 'ジョジョの奇妙な冒険 スティール・ボール・ラン 2nd - 3rd STAGE',
    romaji: 'JoJo no Kimyou na Bouken: Steel Ball Run - 2nd - 3rd STAGE',
    english: "STEEL BALL RUN JoJo's Bizarre Adventure 2nd - 3rd STAGE",
  },
  format: 'ONA',
  seasonYear: 2026,
  startDate: { year: 2026, month: 9, day: 25 },
};
const steelBallRunMatch = matchBangumiCandidates(steelBallRun, [
  { id: 551918, type: 2, name: 'スティール・ボール・ラン ジョジョの奇妙な冒険 1st STAGE', name_cn: '飙马野郎 JOJO的奇妙冒险 第一赛段', date: '2026-03-19', platform: 'WEB' },
  { id: 639938, type: 2, name: 'スティール・ボール・ラン ジョジョの奇妙な冒険 2nd & 3rd STAGE', name_cn: '飙马野郎 JOJO的奇妙冒险 第二&第三赛段', date: '2026-09-25', platform: 'WEB' },
]);
assert.equal(steelBallRunMatch.status, 'matched');
assert.equal(steelBallRunMatch.subjectId, 639938);
assert.equal(steelBallRunMatch.nameCn, '飙马野郎 JOJO的奇妙冒险 第二&第三赛段');

const ranma = {
  id: 209872,
  title: { native: 'らんま1/2 (2024) 第3期', romaji: 'Ranma 1/2 (2024) 3rd Season', english: 'Ranma1/2 (2024) Season 3' },
  format: 'TV', seasonYear: 2026, startDate: { year: 2026, month: 10, day: 4 },
};
assert.ok(bangumiSearchKeywords(ranma).includes('らんま1/2 第3期'));
const ranmaMatch = matchBangumiCandidates(ranma, [
  { id: 489820, type: 2, name: 'らんま1/2', name_cn: '乱马1/2', date: '2024-10-05', platform: 'TV' },
  { id: 637802, type: 2, name: 'らんま1/2 第3期', name_cn: '乱马1/2 第三季', date: '2026-10-03', platform: 'TV' },
]);
assert.equal(ranmaMatch.status, 'matched');
assert.equal(ranmaMatch.subjectId, 637802);

const sasaki = {
  id: 176314,
  title: { native: '佐々木とピーちゃん シーズン２', romaji: 'Sasaki to Pii-chan Season 2', english: 'Sasaki and Peeps Season 2' },
  format: 'TV', seasonYear: 2026, startDate: { year: 2026, month: 10 },
};
const sasakiMatch = matchBangumiCandidates(sasaki, [
  { id: 393038, type: 2, name: '佐々木とピーちゃん', name_cn: '佐佐木与文鸟小哔', date: '2024-01-05', platform: 'TV' },
  { id: 486456, type: 2, name: '佐々木とピーちゃん 第2期', name_cn: '佐佐木与文鸟小哔 第二季', date: null, platform: 'TV' },
]);
assert.equal(sasakiMatch.status, 'matched');
assert.equal(sasakiMatch.subjectId, 486456);

const chitose = {
  id: 198727,
  title: { native: '千歳くんはラムネ瓶のなか 2クール', romaji: 'Chitose-kun wa Ramune Bin no Naka Part 2' },
  format: 'TV', seasonYear: 2026, startDate: { year: 2026, month: 10 },
};
const chitoseMatch = matchBangumiCandidates(chitose, [
  { id: 507634, type: 2, name: '千歳くんはラムネ瓶のなか', name_cn: '弹珠汽水瓶里的千岁同学', date: '2025-10-07', platform: 'TV' },
  { id: 584847, type: 2, name: '千歳くんはラムネ瓶のなか 第2クール', name_cn: '弹珠汽水瓶里的千岁同学 第2部分', date: null, platform: 'TV' },
]);
assert.equal(chitoseMatch.status, 'matched');
assert.equal(chitoseMatch.subjectId, 584847);

const niaListon = {
  id: 206949,
  title: { native: '凶乱令嬢ニア・リストン', romaji: 'Kyouran Reijou Nia Liston' },
  format: 'TV', seasonYear: 2026, startDate: { year: 2026, month: 10 },
};
const niaListonMatch = matchBangumiCandidates(niaListon, [
  { id: 624085, type: 2, name: '凶乱令嬢ニア・リストン 病弱令嬢に転生した神殺しの武人の華麗なる無双録', name_cn: '乱世千金倪亚‧利斯顿转生为娇弱千金的弑神武人华丽无双录', date: null, platform: 'TV' },
]);
assert.equal(niaListonMatch.status, 'matched');
assert.equal(niaListonMatch.subjectId, 624085);

const magicalExplorer = {
  id: 169581,
  title: { native: 'マジカル★エクスプローラー　エロゲの友人キャラに転生したけど、ゲーム知識使って自由に生きる', romaji: 'Magical★Explorer: Eroge no Yuujin Chara ni Tensei Shitakedo, Game Chishiki Tsukatte Jiyuu ni Ikiru' },
  format: 'TV', seasonYear: 2026, startDate: { year: 2026 },
};
const magicalExplorerMatch = matchBangumiCandidates(magicalExplorer, [
  { id: 456078, type: 2, name: 'マジカル★エクスプローラー', name_cn: '魔法★探险家', date: null, platform: 'TV' },
]);
assert.equal(magicalExplorerMatch.status, 'matched');
assert.equal(magicalExplorerMatch.subjectId, 456078);

const diamondAce = {
  id: 213658,
  title: { native: 'ダイヤのA actⅡ -Second Season- 第2クール', romaji: 'Diamond no Ace act II: Second Season Part 2' },
  format: 'TV', seasonYear: 2026, startDate: { year: 2026, month: 10 },
};
assert.ok(bangumiSearchKeywords(diamondAce).includes('ダイヤのA actⅡ'));
const diamondAceMatch = matchBangumiCandidates(diamondAce, [
  { id: 267615, type: 2, name: 'ダイヤのA actⅡ', name_cn: '钻石王牌 act2', date: '2019-04-02', platform: 'TV' },
  { id: 664395, type: 2, name: 'ダイヤのA actⅡ Second Season 第2クール', name_cn: '钻石王牌 act2 第二季 第2部分', date: null, platform: 'TV' },
]);
assert.equal(diamondAceMatch.status, 'matched');
assert.equal(diamondAceMatch.subjectId, 664395);

const tetsuryou = {
  id: 199594,
  title: { native: 'てつりょー！meet with 鉄道むすめ', romaji: 'Tetsuryou! meet with Tetsudou Musume' },
  format: 'TV', seasonYear: 2026, startDate: { year: 2026, month: 10 },
};
const tetsuryouMatch = matchBangumiCandidates(tetsuryou, [
  { id: 590553, type: 2, name: 'てつりょー！meet with 鉄道むすめ', name_cn: '', date: null, platform: 'TV' },
]);
assert.equal(tetsuryouMatch.status, 'unmatched');

async function liveCoverage() {
  const query = `
    query ($page: Int) {
      Page(page: $page, perPage: 50) {
        pageInfo { hasNextPage }
        media(type: ANIME, season: SUMMER, seasonYear: 2026, status_not: CANCELLED, isAdult: false, sort: POPULARITY_DESC) {
          id title { native romaji english } format seasonYear startDate { year month day }
        }
      }
    }
  `;
  const anime = [];
  let page = 1;
  let hasNextPage = true;
  while (hasNextPage && page <= 5) {
    const response = await fetch('https://graphql.anilist.co', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Accept: 'application/json, multipart/mixed',
        Origin: 'https://anilist.co',
        Referer: 'https://anilist.co/',
        'User-Agent': 'AniLog/0.7 (https://github.com/SH1N15/anilog-tracker)',
      },
      body: JSON.stringify({ query, variables: { page } }),
    });
    if (response.status === 403 || response.status === 429 || response.status >= 500) {
      console.warn(`AniList live coverage skipped: upstream HTTP ${response.status}`);
      return;
    }
    assert.equal(response.ok, true);
    const payload = await response.json();
    anime.push(...payload.data.Page.media);
    hasNextPage = payload.data.Page.pageInfo.hasNextPage;
    page += 1;
  }
  const matched = anime.map((item) => ({ item, match: matchOfflineBangumi(item) })).filter(({ match }) => match?.status === 'matched');
  console.log(`Bangumi offline coverage: ${matched.length}/${anime.length} (${Math.round(matched.length / anime.length * 100)}%)`);
  console.log(matched.slice(0, 5).map(({ item, match }) => `${item.title.native} -> ${match.nameCn}`).join('\n'));
}

liveCoverage().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
