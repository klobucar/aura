//
//  AuraApp.swift
//  Aura
//

import SwiftUI

@main
struct AuraApp: App {
    init() {
        // Opt-in OpenTelemetry tracing: only when OTEL_EXPORTER_OTLP_ENDPOINT is
        // set (see docs/OBSERVABILITY.md). Exports operation names and
        // outcomes, never identities or content.
        if (try? telemetryStartFromEnv(serviceName: "aura-macos")) == true {
            NotificationCenter.default.addObserver(
                forName: NSApplication.willTerminateNotification, object: nil, queue: .main
            ) { _ in telemetryShutdown() }
        }
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
                .background {
                    if #available(macOS 26, *) {
                        Color.clear // System handles Liquid Glass chrome
                    } else {
                        VisualEffectBlur(material: .headerView, blendingMode: .behindWindow)
                    }
                }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 960, height: 640)
        .commands {
            CommandGroup(replacing: .newItem) { }
        }
    }
}
