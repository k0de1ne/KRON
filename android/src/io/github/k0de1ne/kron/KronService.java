package io.github.k0de1ne.kron;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.Environment;
import android.os.IBinder;
import java.io.File;
import java.util.Map;

/** Keeps `kron app` (the hub and web UI on 127.0.0.1:7700) running in the background. */
public class KronService extends Service {
    static final String STOP = "stop";
    private volatile boolean running;
    private volatile Process proc;
    private volatile boolean startedWithStorage;
    private static volatile KronService instance;

    static boolean hasStorage(android.content.Context c) {
        if (Build.VERSION.SDK_INT >= 30) return Environment.isExternalStorageManager();
        return c.checkSelfPermission(android.Manifest.permission.WRITE_EXTERNAL_STORAGE)
                == android.content.pm.PackageManager.PERMISSION_GRANTED;
    }

    /** Restart kron when storage access changed, so the agent's /shared moves to Internal storage/kron. */
    static void storageChanged(android.content.Context c) {
        KronService s = instance;
        Process p = s == null ? null : s.proc;
        if (p != null && s.startedWithStorage != hasStorage(c)) p.destroy();
    }

    static File logFile(android.content.Context c) {
        return new File(c.getFilesDir(), "kron.log");
    }

    @Override
    public void onCreate() {
        super.onCreate();
        instance = this;
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel("kron", "kron", NotificationManager.IMPORTANCE_LOW));
        PendingIntent open = PendingIntent.getActivity(this, 0, new Intent(this, MainActivity.class),
                PendingIntent.FLAG_IMMUTABLE);
        PendingIntent stop = PendingIntent.getService(this, 1, new Intent(this, KronService.class).setAction(STOP),
                PendingIntent.FLAG_IMMUTABLE);
        Notification n = new Notification.Builder(this, "kron")
                .setSmallIcon(R.drawable.ic_stat)
                .setContentTitle("kron работает")
                .setContentText("Агенты и чаты работают на телефоне")
                .setContentIntent(open)
                .addAction(new Notification.Action.Builder(null, "Остановить", stop).build())
                .setOngoing(true)
                .build();
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(1, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);
        } else {
            startForeground(1, n);
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null && STOP.equals(intent.getAction())) {
            stopSelf();
            return START_NOT_STICKY;
        }
        if (!running) {
            running = true;
            new Thread(this::loop, "kron").start();
        }
        return START_STICKY;
    }

    private void loop() {
        File home = getFilesDir();
        File work = new File(home, "work");
        work.mkdirs();
        File exe = new File(getApplicationInfo().nativeLibraryDir, "libkron.so");
        while (running) {
            try {
                File log = logFile(this);
                if (log.length() > 1_000_000) log.delete();
                ProcessBuilder pb = new ProcessBuilder(exe.getAbsolutePath(), "app")
                        .directory(work)
                        .redirectErrorStream(true)
                        .redirectOutput(ProcessBuilder.Redirect.appendTo(log));
                Map<String, String> env = pb.environment();
                env.put("HOME", home.getAbsolutePath());
                env.put("TMPDIR", getCacheDir().getAbsolutePath());
                env.put("SHELL", "/system/bin/sh");
                // Own port, so a kron started by hand (e.g. in Termux) cannot shadow the app.
                env.put("KRON_LISTEN", "127.0.0.1:" + MainActivity.PORT);
                env.put("HOSTNAME", Build.MODEL.toLowerCase().replaceAll("[^a-z0-9]+", "-"));
                // proot and the agent's Ubuntu (see src/sandbox.rs).
                env.put("KRON_NATIVE_DIR", getApplicationInfo().nativeLibraryDir);
                startedWithStorage = hasStorage(this);
                proc = pb.start();
                proc.waitFor();
            } catch (Exception e) {
                // Logged by the UI's error page through kron.log; retry below.
            }
            if (running) {
                try {
                    Thread.sleep(3000);
                } catch (InterruptedException e) {
                    return;
                }
            }
        }
    }

    @Override
    public void onDestroy() {
        running = false;
        instance = null;
        Process p = proc;
        if (p != null) p.destroy();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
