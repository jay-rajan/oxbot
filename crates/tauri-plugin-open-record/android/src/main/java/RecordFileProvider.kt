package dev.oxbot.openrecord

import androidx.core.content.FileProvider

// The plugin's own provider: the app has Android's FileProvider already,
// and one class can be listed only once, so this one is its own.
class RecordFileProvider : FileProvider()
