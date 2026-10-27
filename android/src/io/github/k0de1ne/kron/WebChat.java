package io.github.k0de1ne.kron;

import android.app.Activity;
import android.content.Context;
import android.content.MutableContextWrapper;
import android.util.DisplayMetrics;
import android.view.View;
import android.view.ViewGroup;
import android.webkit.CookieManager;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;

/**
 * The WebView that serves ChatGPT / Gemini web chats on the phone. kron drives it over
 * DevTools (KRON_WEBVIEW is its socket); the user signs in to the sites in this same view.
 * It lives as long as the app process; the activity shows it only for sign-in.
 */
final class WebChat {
    private static WebView view;
    private static MutableContextWrapper ctx;

    /** This process's WebView DevTools socket; only this app's uid (and adb) may connect to it. */
    static String socket() {
        return "webview_devtools_remote_" + android.os.Process.myPid();
    }

    /** Must run on the main thread. */
    static WebView get(Context c) {
        if (view != null) return view;
        WebView.setWebContentsDebuggingEnabled(true);
        ctx = new MutableContextWrapper(c.getApplicationContext());
        view = new WebView(ctx);
        WebSettings s = view.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        s.setSupportMultipleWindows(false); // pop-ups ("Sign in with Google") open in place
        // Look like mobile Chrome: Google refuses to sign in inside a WebView ("; wv").
        s.setUserAgentString(s.getUserAgentString().replace("; wv", "").replaceAll("Version/[\\d.]+ ", ""));
        CookieManager.getInstance().setAcceptCookie(true);
        CookieManager.getInstance().setAcceptThirdPartyCookies(view, true);
        view.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView v, WebResourceRequest req) {
                String scheme = req.getUrl().getScheme();
                // intent:, market: and the like have nothing to open here.
                return !"http".equals(scheme) && !"https".equals(scheme);
            }
        });
        // A real viewport even while the view is not on screen.
        DisplayMetrics dm = c.getResources().getDisplayMetrics();
        view.measure(View.MeasureSpec.makeMeasureSpec(dm.widthPixels, View.MeasureSpec.EXACTLY),
                View.MeasureSpec.makeMeasureSpec(dm.heightPixels, View.MeasureSpec.EXACTLY));
        view.layout(0, 0, dm.widthPixels, dm.heightPixels);
        view.loadUrl("about:blank");
        return view;
    }

    /** Put the view into the activity's layout, below everything else. */
    static WebView attach(Activity a, ViewGroup parent, ViewGroup.LayoutParams lp) {
        WebView v = get(a);
        detach();
        ctx.setBaseContext(a); // dialogs and <select> pop-ups need an activity
        parent.addView(v, 0, lp);
        return v;
    }

    static void detach() {
        if (view == null) return;
        if (view.getParent() != null) ((ViewGroup) view.getParent()).removeView(view);
        ctx.setBaseContext(ctx.getApplicationContext());
    }

    private WebChat() {}
}
