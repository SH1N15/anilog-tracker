package io.anilog.android

import android.Manifest
import android.app.Activity
import android.app.AlarmManager
import android.content.Intent
import android.os.Build
import android.provider.Settings
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import android.content.pm.PackageManager
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import org.json.JSONArray
import org.json.JSONObject

@TauriPlugin
class AniLogPlugin(private val activity: Activity) : Plugin(activity) {
  private val webDav = WebDavClient()

  override fun load(webView: android.webkit.WebView) {
    NotificationScheduler.ensureChannel(activity.applicationContext)
    DailyTaskReminderScheduler.ensureChannel(activity.applicationContext)
    BackgroundSync.schedulePeriodic(activity.applicationContext)
  }

  @Command
  fun configure(invoke: Invoke) {
    // rc.4 问题 3：配置桥接是冷启动最重的 JNI 往返（快照写入 + 逐番重排
    // 闹钟 + 缓存投影），耗时写进 logcat 供真机定位"正在读取本地数据"卡顿。
    val startedAt = android.os.SystemClock.elapsedRealtime()
    try {
      val args = invoke.getArgs()
      val before = MobileStore.following(activity.applicationContext)
      val following = args.optJSONArray("following") ?: JSONArray()
      val pendingTasks = args.optJSONArray("pendingTasks") ?: JSONArray()
      MobileStore.configure(
        activity.applicationContext,
        following,
        pendingTasks,
        args.optJSONObject("followingDeletedAt") ?: JSONObject(),
        args.optBoolean("notificationsEnabled", true),
        args.optBoolean("createTasksEnabled", true),
        args.optBoolean("dailyTaskReminderEnabled", false),
        args.optString("dailyTaskReminderTime", "20:00"),
        args.optString("uiLanguage", "zh-CN")
      )
      MobileStore.setBangumiApiBaseUrl(activity.applicationContext, if (BuildConfig.isOriginalEdition) "" else args.optString("bangumiApiBaseUrl", ""))
      MobileStore.setPullCollectionsEnabled(activity.applicationContext, !BuildConfig.isOriginalEdition && args.optBoolean("pullCollections", false))
      val caches = args.optJSONArray("anilistCache") ?: JSONArray()
      for (index in 0 until caches.length()) {
        val media = caches.optJSONObject(index) ?: continue
        val fetchedAt = media.optLong("_fetchedAt")
        if (fetchedAt > 0 && fetchedAt <= System.currentTimeMillis() / 1000L) {
          MobileStore.setAnilistScheduleCache(activity.applicationContext, media, fetchedAt)
        }
      }
      AniListScheduler.refreshCachedSchedules(activity.applicationContext)
      val current = MobileStore.following(activity.applicationContext)
      val currentIds = (0 until current.length()).mapNotNull { current.optJSONObject(it)?.optInt("id") }.toSet()
      for (index in 0 until before.length()) {
        before.optJSONObject(index)?.optInt("id")?.takeIf { it !in currentIds }?.let { NotificationScheduler.cancel(activity.applicationContext, it) }
      }
      NotificationScheduler.scheduleAll(activity.applicationContext)
      DailyTaskReminderScheduler.schedule(activity.applicationContext, false)
      BackgroundSync.schedulePeriodic(activity.applicationContext)
      if (args.optBoolean("notificationsEnabled", true) && Build.VERSION.SDK_INT >= 33 && ContextCompat.checkSelfPermission(activity, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
        ActivityCompat.requestPermissions(activity, arrayOf(Manifest.permission.POST_NOTIFICATIONS), 41001)
      }
      android.util.Log.i("AniLog", "configure bridge finished in " + (android.os.SystemClock.elapsedRealtime() - startedAt) + "ms")
      invoke.resolve(status(JSONArray(), false))
    } catch (error: Exception) { invoke.reject(error.message, error) }
  }

  @Command
  fun consumeEvents(invoke: Invoke) { invoke.resolve(status(MobileStore.consumeEvents(activity.applicationContext), true)) }

  /** rc.4 问题 1（B）：Rust 用户动作（任务勾选/追番变更）后调用，排一个
   *  WorkManager 一次性同步，进程被厂商省电杀掉后仍能完成上传合并。 */
  @Command
  fun enqueueImmediateSync(invoke: Invoke) {
    BackgroundSync.enqueueImmediate(activity.applicationContext)
    invoke.resolve()
  }

  @Command
  fun getLegacyState(invoke: Invoke) {
    invoke.resolve(JSObject()
      .put("following", MobileStore.following(activity.applicationContext))
      .put("pendingTasks", MobileStore.pendingTasks(activity.applicationContext))
      .put("settings", JSObject()
        .put("notifyWhenAired", MobileStore.notificationsEnabled(activity.applicationContext))
        .put("createWatchTasks", MobileStore.createTasksEnabled(activity.applicationContext))
        .put("dailyTaskReminderEnabled", MobileStore.dailyTaskReminderEnabled(activity.applicationContext))
        .put("dailyTaskReminderTime", MobileStore.dailyTaskReminderTime(activity.applicationContext))
        .put("uiLanguage", MobileStore.uiLanguage(activity.applicationContext))))
  }

  @Command
  fun syncNow(invoke: Invoke) {
    val args = invoke.getArgs()
    val force = args.optBoolean("force", false)
    val target = args.optInt("target", 0)
    Thread {
      try {
        val updated = AniListScheduler.sync(activity.applicationContext, force, target)
        invoke.resolve(status(MobileStore.consumeEvents(activity.applicationContext), false).put("updated", updated))
      } catch (error: Exception) { invoke.reject("AniList 同步失败：${error.message}", error) }
    }.start()
  }

  @Command
  fun requestExactScheduling(invoke: Invoke) {
    if (Build.VERSION.SDK_INT >= 31) {
      val alarms = activity.getSystemService(AlarmManager::class.java)
      if (!alarms.canScheduleExactAlarms()) activity.startActivity(Intent(Settings.ACTION_REQUEST_SCHEDULE_EXACT_ALARM, android.net.Uri.parse("package:${activity.packageName}")))
    }
    invoke.resolve()
  }

  @Command
  fun getWebDavConfig(invoke: Invoke) { invoke.resolve(WebDavStore.publicConfig(activity.applicationContext)) }

  @Command
  fun saveWebDavConfig(invoke: Invoke) {
    try {
      val args = invoke.getArgs()
      val replacePassword = args.has("password")
      val password = args.optString("password", "")
      WebDavStore.save(activity.applicationContext, args.optBoolean("enabled", false), args.optString("baseUrl", ""), args.optString("username", "").trim(), password, replacePassword)
      invoke.resolve(WebDavStore.publicConfig(activity.applicationContext))
    } catch (error: Exception) { invoke.reject(error.message, error) }
  }

  @Command
  fun testWebDavConnection(invoke: Invoke) { Thread { try { webDav.test(WebDavStore.load(activity.applicationContext)); invoke.resolve(JSObject().put("ok", true).put("message", "WebDAV 连接成功")) } catch (error: Exception) { invoke.reject(error.message, error) } }.start() }

  @Command
  fun webDavDownload(invoke: Invoke) { Thread { try { val result = webDav.download(WebDavStore.load(activity.applicationContext)); invoke.resolve(JSObject().put("found", result.found).put("etag", result.etag).put("body", result.body)) } catch (error: Exception) { invoke.reject(error.message, error) } }.start() }

  @Command
  fun webDavUpload(invoke: Invoke) { Thread { try { val args = invoke.getArgs(); val uploaded = webDav.upload(WebDavStore.load(activity.applicationContext), args.optString("body", ""), args.optBoolean("remoteFound", false), args.optString("etag", "")); invoke.resolve(JSObject().put("ok", uploaded).put("conflict", !uploaded)) } catch (error: Exception) { invoke.reject(error.message, error) } }.start() }

  @Command
  fun finishWebDavSync(invoke: Invoke) {
    val args = invoke.getArgs()
    WebDavStore.finishSync(activity.applicationContext, if (args.has("error")) args.optString("error") else null)
    invoke.resolve(WebDavStore.publicConfig(activity.applicationContext))
  }

  @Command
  fun bangumiGetToken(invoke: Invoke) { invoke.resolve(JSObject().put("token", BangumiTokenStore.load(activity.applicationContext) ?: JSONObject.NULL)) }

  @Command
  fun bangumiSaveToken(invoke: Invoke) {
    val token = invoke.getArgs().optString("token", "")
    invoke.resolve(JSObject().put("ok", BangumiTokenStore.save(activity.applicationContext, token)))
  }

  @Command
  fun bangumiClearToken(invoke: Invoke) { invoke.resolve(JSObject().put("ok", BangumiTokenStore.clear(activity.applicationContext))) }

  private fun status(events: JSONArray, consumeOpenTasks: Boolean): JSObject {
    val alarms = activity.getSystemService(AlarmManager::class.java)
    return JSObject()
      .put("granted", Build.VERSION.SDK_INT < 33 || ContextCompat.checkSelfPermission(activity, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED)
      .put("exactSchedulingGranted", Build.VERSION.SDK_INT < 31 || alarms.canScheduleExactAlarms())
      .put("events", events)
      .put("following", MobileStore.following(activity.applicationContext))
      .put("anilistCache", MobileStore.exportAnilistCache(activity.applicationContext))
      .put("document", BackgroundSyncWorker.localDocument(activity.applicationContext))
      .put("syncedAt", MobileStore.lastSyncAt(activity.applicationContext))
      .put("warning", MobileStore.anilistSyncWarning(activity.applicationContext))
      .put("openTasks", consumeOpenTasks && MobileStore.consumeOpenTasks(activity.applicationContext))
  }
}
