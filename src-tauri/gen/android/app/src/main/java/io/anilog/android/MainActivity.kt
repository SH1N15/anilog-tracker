package io.anilog.android

import android.content.Intent
import android.os.Bundle
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    prepareBackground(intent)
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    prepareBackground(intent)
  }

  private fun prepareBackground(intent: Intent?) {
    NotificationScheduler.ensureChannel(applicationContext)
    DailyTaskReminderScheduler.ensureChannel(applicationContext)
    if (intent?.getBooleanExtra("openTasks", false) == true) MobileStore.requestOpenTasks(applicationContext)
    NotificationScheduler.scheduleAll(applicationContext)
    // rc.4 问题 4：开 app 不再补发 20:00 每日汇总（checkMissed=false）。
    // 用户已在看界面，补发与正点文案相同的汇总只会被感知为重复通知；
    // 漏发兜底保留在 BootReceiver（开机后设备重启仍属"错过"场景）。
    DailyTaskReminderScheduler.schedule(applicationContext, false)
    BackgroundSync.schedulePeriodic(applicationContext)
  }
}
