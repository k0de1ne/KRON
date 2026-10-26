package io.github.k0de1ne.kron;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;
import java.io.File;
import java.util.Map;

/** Keeps `kron app` (the hub and web UI on 127.0.0.1:7700) running in the background. */
public class KronService extends Service {
    static final String STOP = "stop";
    private volatile boolean running;
    private volatile Process proc;

    static File logFile(android.content.Context c) {
        return new File(c.getFilesDir(), "kron.log");
    }

    @Override
    public void onCreate() {
        super.onCreate();
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel("kron", "kron", NotificationManager.IMPORTANCE_LOW));
        PendingIntent open = PendingIntent.getActivity(this, 0, new Intent(this, MainActivity.class),
                PendingIntent.FLAG_IMMUTABLE);
        PendingIntent stop = PendingIntent.getService(this, 1, new Intent(this, KronService.class).setAction(STOP),
                PendingIntent.FLAG_IMMUTABLE);
        Notification n = new Notification.Builder(this, "kron")
                .setSmallIcon(R.drawable.ic_stat)
                .setContentTitle("kron работает")
                .setContentText("Агенты и чаты доступны на 127.0.0.1:7700")
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
                env.put("HOSTNAME", Build.MODEL.toLowerCase().replaceAll("[^a-z0-9]+", "-"));
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
        Process p = proc;
        if (p != null) p.destroy();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
