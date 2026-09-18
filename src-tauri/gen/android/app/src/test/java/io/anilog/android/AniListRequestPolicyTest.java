package io.anilog.android;

import static org.junit.Assert.*;
import java.util.Map;
import java.util.Arrays;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

public class AniListRequestPolicyTest {
    @Test
    public void queryUsesOnlyArgumentsSupportedByMediaAiringSchedule() {
        Matcher fields = Pattern.compile("airingSchedule\\s*\\(([^)]*)\\)").matcher(AniListScheduler.QUERY);
        int count = 0;
        while (fields.find()) {
            for (String argument : fields.group(1).split(",")) {
                String name = argument.split(":")[0].trim();
                assertTrue("Unsupported Media.airingSchedule argument: " + name,
                    Arrays.asList("notYetAired", "page", "perPage").contains(name));
            }
            count++;
        }
        assertEquals(2, count);
    }

    @Test
    public void mappingAndTrackingDetermineRequestFrequency() throws Exception {
        JSONArray following = new JSONArray()
            .put(new JSONObject().put("id", 1).put("source", "bangumi"))
            .put(new JSONObject().put("id", 2).put("source", "bangumi").put("anilistId", 100).put("bangumiStatus", "wish"))
            .put(new JSONObject().put("id", 3).put("source", "bangumi").put("anilistId", 100).put("bangumiStatus", "doing"))
            .put(new JSONObject().put("id", 4).put("source", "bangumi").put("anilistId", 200).put("bangumiStatus", "done"))
            .put(new JSONObject().put("id", 5).put("source", "bangumi").put("anilistId", 300).put("bangumiStatus", "dropped"));
        Map<Integer, Long> requests = AniListRequestPolicy.requests(following, false, 21600, 0);
        assertEquals(2, requests.size());
        assertEquals(Long.valueOf(21600), requests.get(100));
        assertEquals(Long.valueOf(30L * 86400), requests.get(200));
        assertEquals(1, AniListRequestPolicy.requests(following, false, 21600, 3).size());
        assertEquals(5, AniListRequestPolicy.requests(following, true, 21600, 0).size());
    }

    @Test
    public void forceUsesShortCooldownAndDoesNotRenewPrecision() throws Exception {
        JSONObject cached = new JSONObject().put("id", 1).put("_fetchedAt", 100);
        assertFalse(AniListRequestPolicy.needsRefresh(cached, 159, 86400, true));
        assertTrue(AniListRequestPolicy.needsRefresh(cached, 160, 86400, true));
        assertFalse(AniListRequestPolicy.needsRefresh(cached, 160, 86400, false));
        assertEquals(100, cached.getLong("_fetchedAt"));
        assertFalse(AniListRequestPolicy.fresh(100, 100 + 7L * 86400, 7L * 86400));
        assertFalse(AniListRequestPolicy.fresh(200, 100, 86400));
    }

    @Test
    public void missingAndFinishedMediaHaveLongerCache() throws Exception {
        JSONObject missing = new JSONObject().put("_fetchedAt", 100).put("_missing", true);
        assertFalse(AniListRequestPolicy.needsRefresh(missing, 1000, 300, false));
        JSONObject finished = new JSONObject().put("_fetchedAt", 100).put("status", "FINISHED");
        assertFalse(AniListRequestPolicy.needsRefresh(finished, 100 + 20L * 86400, 300, false));
        assertTrue(AniListRequestPolicy.needsRefresh(finished, 1000, 300, true));
    }

    @Test
    public void backoffHonorsServerDelay() {
        assertEquals(120, AniListRequestPolicy.retryDelay(429, "120", 1, 0));
        assertEquals(900, AniListRequestPolicy.retryDelay(403, null, 1, 0));
        assertEquals(120, AniListRequestPolicy.retryDelay(503, null, 2, 0));
        assertEquals(3600, AniListRequestPolicy.retryDelay(503, null, 100, 0));
    }

    @Test
    public void nextEpisodeUsesOnlyKnownFutureInstants() throws Exception {
        JSONObject media = new JSONObject().put("nextAiringEpisode", new JSONObject().put("episode", 5).put("airingAt", 100))
            .put("futureAiringSchedule", new JSONObject().put("nodes", new JSONArray()
                .put(new JSONObject().put("episode", 6).put("airingAt", 200))));
        assertEquals(6, AniListRequestPolicy.nextEpisode(media, 150).getInt("episode"));
        assertNull(AniListRequestPolicy.nextEpisode(media, 201));
    }

    @Test
    public void responseCannotPolluteAnUnrequestedId() throws Exception {
        JSONArray response = AniListRequestPolicy.validResponse(new JSONArray().put(1).put(2),
            new JSONArray().put(new JSONObject().put("id", 1)).put(new JSONObject().put("id", 99)));
        assertEquals(2, response.length());
        assertEquals(2, response.getJSONObject(1).getInt("id"));
        assertTrue(response.getJSONObject(1).getBoolean("_missing"));
    }
}
