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
      invoke.resolve(status(JSONArray(), false))
    } catch (error: Exception) { invoke.reject(error.message, error) }
  }

  @Command
  fun consumeEvents(invoke: Invoke) { invoke.resolve(status(MobileStore.consumeEvents(activity.applicationContext), true)) }

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
    Thread {
      try {
        val updated = AniListScheduler.sync(activity.applicationContext)
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
      .put("document", BackgroundSyncWorker.localDocument(activity.applicationContext))
      .put("syncedAt", MobileStore.lastSyncAt(activity.applicationContext))
      .put("openTasks", consumeOpenTasks && MobileStore.consumeOpenTasks(activity.applicationContext))
  }
}
