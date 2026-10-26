package io.github.k0de1ne.kron;

import android.Manifest;
import android.app.Activity;
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
    private WebView web;
    private int retries;

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
        web.setWebChromeClient(new WebChromeClient()); // alert() / confirm()
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

    private void styleBars() {
        boolean dark = (getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK)
                == Configuration.UI_MODE_NIGHT_YES;
        int bg = dark ? Color.parseColor("#262624") : Color.parseColor("#faf9f5");
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
