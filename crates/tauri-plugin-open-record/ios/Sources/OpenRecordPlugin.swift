import QuickLook
import SwiftRs
import Tauri
import UIKit

struct OpenArgs: Decodable {
  let path: String
}

// Shows a kept record in Quick Look, the iPhone's own viewer for documents,
// pictures and more; its Share button sends it to another app.
class OpenRecordPlugin: Plugin, QLPreviewControllerDataSource {
  private var shown: URL?

  @objc public func open(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(OpenArgs.self)
    let url = URL(fileURLWithPath: args.path)
    guard FileManager.default.isReadableFile(atPath: url.path) else {
      invoke.reject("That record isn't on this phone.")
      return
    }
    DispatchQueue.main.async {
      self.shown = url
      let preview = QLPreviewController()
      preview.dataSource = self
      self.manager.viewController?.present(preview, animated: true)
      invoke.resolve()
    }
  }

  func numberOfPreviewItems(in controller: QLPreviewController) -> Int {
    shown == nil ? 0 : 1
  }

  func previewController(_ controller: QLPreviewController, previewItemAt index: Int)
    -> QLPreviewItem
  {
    (shown ?? URL(fileURLWithPath: "/")) as NSURL
  }
}

@_cdecl("init_plugin_open_record")
func initPlugin() -> Plugin {
  return OpenRecordPlugin()
}
