import SwiftUI
import WebKit

struct WebView: NSViewRepresentable {
    let htmlContent: String

    func makeNSView(context: Context) -> WKWebView {
        let webView = WKWebView()
        webView.setValue(false, forKey: "drawsBackground") // Transparent
        return webView
    }

    func updateNSView(_ nsView: WKWebView, context: Context) {
        nsView.loadHTMLString(htmlContent, baseURL: nil)
    }
}
