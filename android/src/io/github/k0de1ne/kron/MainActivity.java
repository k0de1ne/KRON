package io.github.k0de1ne.kron;

import android.Manifest;
import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.ClipData;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.res.Configuration;
import android.graphics.Color;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.PowerManager;
import android.provider.Settings;
import android.view.View;
import android.webkit.JavascriptInterface;
import android.webkit.ValueCallback;
import android.webkit.WebChromeClient;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import java.io.RandomAccessFile;

/** The kron chat UI in a WebView, served by KronService on this phone. */
public class MainActivity extends Activity {
    static final int PORT = 7790;
    static final String UI = "http://127.0.0.1:" + PORT + "/";
    static final int PICK_FILES = 7;
    private WebView web;
    private int retries;
    private ValueCallback<Uri[]> fileCallback;
    /** Bar colors chosen by the UI's theme setting (null: follow the system). */
    private String barsColor;
    private boolean barsDark;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        startForegroundService(new Intent(this, KronService.class));
        askPermissions();
        styleBars();

        web = new WebView(this);
        WebSettings s = web.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        s.setUserAgentString(s.getUserAgentString() + " kron-android");
        web.addJavascriptInterface(new Bridge(), "KronAndroid");
        web.setWebChromeClient(new WebChromeClient() {
            // Attachments and uploads: <input type=file> opens the system picker.
            @Override
            public boolean onShowFileChooser(WebView view, ValueCallback<Uri[]> callback, FileChooserParams params) {
                if (fileCallback != null) fileCallback.onReceiveValue(null);
                fileCallback = callback;
                Intent pick = params.createIntent();
                if (params.getMode() == FileChooserParams.MODE_OPEN_MULTIPLE) {
                    pick.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
                }
                try {
                    startActivityForResult(pick, PICK_FILES);
                } catch (Exception e) {
                    fileCallback = null;
                    return false;
                }
                return true;
            }
        });
        web.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest req) {
                Uri u = req.getUrl();
                if ("127.0.0.1".equals(u.getHost()) && u.getPort() == PORT) return false;
                // Sign-in pages and links open in the real browser; its redirect to
                // localhost still reaches kron on this phone.
                startActivity(new Intent(Intent.ACTION_VIEW, u));
                return true;
            }

            @Override
            public void onReceivedError(WebView view, WebResourceRequest req, WebResourceError err) {
                if (!req.isForMainFrame()) return;
                // The server is still starting (or restarting): try again shortly.
                if (++retries < 40) {
                    view.postDelayed(() -> view.loadUrl(UI), 500);
                } else {
                    showLog(view);
                }
            }

            @Override
            public void onPageFinished(WebView view, String url) {
                if (url.startsWith(UI)) retries = 0;
            }
        });
        setContentView(web);
        if (state == null || web.restoreState(state) == null) web.loadUrl(UI);
    }

    private void showLog(WebView view) {
        String tail = "";
        try (RandomAccessFile f = new RandomAccessFile(KronService.logFile(this), "r")) {
            long start = Math.max(0, f.length() - 8000);
            byte[] b = new byte[(int) (f.length() - start)];
            f.seek(start);
            f.readFully(b);
            tail = new String(b, "UTF-8");
        } catch (Exception e) {
            tail = e.toString();
        }
        String html = "<meta name=viewport content='width=device-width'><body style='font-family:sans-serif;padding:16px'>"
                + "<h3>kron не запустился</h3><p><a href='" + UI + "'>Повторить</a></p><pre style='white-space:pre-wrap'>"
                + android.text.Html.escapeHtml(tail) + "</pre>";
        retries = 0;
        view.loadDataWithBaseURL(UI, html, "text/html", "UTF-8", null);
    }

    private void askPermissions() {
        if (Build.VERSION.SDK_INT >= 33) requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, 1);
        if (Build.VERSION.SDK_INT < 30) requestPermissions(new String[] {Manifest.permission.WRITE_EXTERNAL_STORAGE}, 2);
        SharedPreferences p = getSharedPreferences("kron", MODE_PRIVATE);
        // All files access makes Internal storage/kron the agent's /shared folder.
        if (Build.VERSION.SDK_INT >= 30 && !p.getBoolean("askedStorage", false) && !KronService.hasStorage(this)) {
            p.edit().putBoolean("askedStorage", true).apply();
            try {
                startActivity(new Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
                        Uri.parse("package:" + getPackageName())));
            } catch (Exception ignored) {
            }
        }
        PowerManager pm = getSystemService(PowerManager.class);
        if (!p.getBoolean("askedBattery", false) && !pm.isIgnoringBatteryOptimizations(getPackageName())) {
            p.edit().putBoolean("askedBattery", true).apply();
            try {
                startActivity(new Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                        Uri.parse("package:" + getPackageName())));
            } catch (Exception ignored) {
            }
        }
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request != PICK_FILES || fileCallback == null) return;
        Uri[] uris = null;
        if (result == RESULT_OK && data != null) {
            ClipData clip = data.getClipData();
            if (clip != null) {
                uris = new Uri[clip.getItemCount()];
                for (int i = 0; i < uris.length; i++) uris[i] = clip.getItemAt(i).getUri();
            } else if (data.getData() != null) {
                uris = new Uri[] {data.getData()};
            }
        }
        fileCallback.onReceiveValue(uris);
        fileCallback = null;
    }

    /** window.KronAndroid in the web UI. */
    class Bridge {
        /** A notification when an answer is ready or the agent waits, while the app is in the background. */
        @JavascriptInterface
        public void notice(String title, String text) {
            NotificationManager nm = getSystemService(NotificationManager.class);
            nm.createNotificationChannel(
                    new NotificationChannel("kron-events", "Ответы и вопросы", NotificationManager.IMPORTANCE_DEFAULT));
            PendingIntent open = PendingIntent.getActivity(MainActivity.this, 0,
                    new Intent(MainActivity.this, MainActivity.class), PendingIntent.FLAG_IMMUTABLE);
            Notification n = new Notification.Builder(MainActivity.this, "kron-events")
                    .setSmallIcon(R.drawable.ic_stat)
                    .setContentTitle(title)
                    .setContentText(text)
                    .setStyle(new Notification.BigTextStyle().bigText(text))
                    .setContentIntent(open)
                    .setAutoCancel(true)
                    .build();
            nm.notify(2, n);
        }

        /** Downloads from the UI (WebView cannot save blob: links): into the phone's Download folder. */
        @JavascriptInterface
        public String saveFile(String name, String base64) {
            byte[] data = android.util.Base64.decode(base64, android.util.Base64.DEFAULT);
            try {
                if (Build.VERSION.SDK_INT >= 29) {
                    android.content.ContentValues v = new android.content.ContentValues();
                    v.put(android.provider.MediaStore.MediaColumns.DISPLAY_NAME, name);
                    Uri uri = getContentResolver().insert(android.provider.MediaStore.Downloads.EXTERNAL_CONTENT_URI, v);
                    try (java.io.OutputStream out = getContentResolver().openOutputStream(uri)) {
                        out.write(data);
                    }
                } else {
                    java.io.File dir = android.os.Environment.getExternalStoragePublicDirectory(
                            android.os.Environment.DIRECTORY_DOWNLOADS);
                    try (java.io.FileOutputStream out = new java.io.FileOutputStream(new java.io.File(dir, name))) {
                        out.write(data);
                    }
                }
                return "";
            } catch (Exception e) {
                return e.toString();
            }
        }

        /** The UI's theme (light / dark / system) colors the status and navigation bars. */
        @JavascriptInterface
        public void setBars(String color, boolean dark) {
            runOnUiThread(() -> {
                barsColor = color;
                barsDark = dark;
                styleBars();
            });
        }
    }

    private void styleBars() {
        boolean dark = (getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK)
                == Configuration.UI_MODE_NIGHT_YES;
        int bg = dark ? Color.parseColor("#1f1e1d") : Color.parseColor("#faf9f5");
        if (barsColor != null) {
            try {
                bg = Color.parseColor(barsColor);
                dark = barsDark;
            } catch (IllegalArgumentException ignored) {
            }
        }
        getWindow().setStatusBarColor(bg);
        getWindow().setNavigationBarColor(bg);
        int flags = dark ? 0 : View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
        getWindow().getDecorView().setSystemUiVisibility(flags);
    }

    @Override
    protected void onResume() {
        super.onResume();
        KronService.storageChanged(this);
    }

    @Override
    public void onConfigurationChanged(Configuration c) {
        super.onConfigurationChanged(c);
        styleBars();
    }

    @Override
    protected void onSaveInstanceState(Bundle out) {
        super.onSaveInstanceState(out);
        web.saveState(out);
    }

    @Override
    public void onBackPressed() {
        if (web.canGoBack()) web.goBack();
        else moveTaskToBack(true);
    }
}
