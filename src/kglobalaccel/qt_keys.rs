// Generated from Qt 6 (qtbase src/gui/platform/unix/qxkbcommon.cpp KeyTbl and
// src/gui/kernel/qkeysequence.cpp keyname[]) and qnamespace.h key values.
// Do not edit by hand.

/// (xkb keysym, Qt::Key) pairs, in Qt's order (first match wins for each direction).
pub static XKB_TO_QT: &[(u32, u32)] = &[
    (0xff1b, 0x1000000), // Escape -> Key_Escape
    (0xff09, 0x1000001), // Tab -> Key_Tab
    (0xfe20, 0x1000002), // ISO_Left_Tab -> Key_Backtab
    (0xff08, 0x1000003), // BackSpace -> Key_Backspace
    (0xff0d, 0x1000004), // Return -> Key_Return
    (0xff63, 0x1000006), // Insert -> Key_Insert
    (0xffff, 0x1000007), // Delete -> Key_Delete
    (0xff0b, 0x1000007), // Clear -> Key_Delete
    (0xff13, 0x1000008), // Pause -> Key_Pause
    (0xff61, 0x1000009), // Print -> Key_Print
    (0xff15, 0x100000a), // Sys_Req -> Key_SysReq
    (0xff50, 0x1000010), // Home -> Key_Home
    (0xff57, 0x1000011), // End -> Key_End
    (0xff51, 0x1000012), // Left -> Key_Left
    (0xff52, 0x1000013), // Up -> Key_Up
    (0xff53, 0x1000014), // Right -> Key_Right
    (0xff54, 0x1000015), // Down -> Key_Down
    (0xff55, 0x1000016), // Prior -> Key_PageUp
    (0xff56, 0x1000017), // Next -> Key_PageDown
    (0xffe1, 0x1000020), // Shift_L -> Key_Shift
    (0xffe2, 0x1000020), // Shift_R -> Key_Shift
    (0xffe6, 0x1000020), // Shift_Lock -> Key_Shift
    (0xffe3, 0x1000021), // Control_L -> Key_Control
    (0xffe4, 0x1000021), // Control_R -> Key_Control
    (0xffe7, 0x1000022), // Meta_L -> Key_Meta
    (0xffe8, 0x1000022), // Meta_R -> Key_Meta
    (0xffe9, 0x1000023), // Alt_L -> Key_Alt
    (0xffea, 0x1000023), // Alt_R -> Key_Alt
    (0xffe5, 0x1000024), // Caps_Lock -> Key_CapsLock
    (0xff7f, 0x1000025), // Num_Lock -> Key_NumLock
    (0xff14, 0x1000026), // Scroll_Lock -> Key_ScrollLock
    (0xffeb, 0x1000053), // Super_L -> Key_Super_L
    (0xffec, 0x1000054), // Super_R -> Key_Super_R
    (0xff67, 0x1000055), // Menu -> Key_Menu
    (0xffed, 0x1000056), // Hyper_L -> Key_Hyper_L
    (0xffee, 0x1000057), // Hyper_R -> Key_Hyper_R
    (0xff6a, 0x1000058), // Help -> Key_Help
    (0xff80, 0x20), // KP_Space -> Key_Space
    (0xff89, 0x1000001), // KP_Tab -> Key_Tab
    (0xff8d, 0x1000005), // KP_Enter -> Key_Enter
    (0xff95, 0x1000010), // KP_Home -> Key_Home
    (0xff96, 0x1000012), // KP_Left -> Key_Left
    (0xff97, 0x1000013), // KP_Up -> Key_Up
    (0xff98, 0x1000014), // KP_Right -> Key_Right
    (0xff99, 0x1000015), // KP_Down -> Key_Down
    (0xff9a, 0x1000016), // KP_Prior -> Key_PageUp
    (0xff9b, 0x1000017), // KP_Next -> Key_PageDown
    (0xff9c, 0x1000011), // KP_End -> Key_End
    (0xff9d, 0x100000b), // KP_Begin -> Key_Clear
    (0xff9e, 0x1000006), // KP_Insert -> Key_Insert
    (0xff9f, 0x1000007), // KP_Delete -> Key_Delete
    (0xffbd, 0x3d), // KP_Equal -> Key_Equal
    (0xffaa, 0x2a), // KP_Multiply -> Key_Asterisk
    (0xffab, 0x2b), // KP_Add -> Key_Plus
    (0xffac, 0x2c), // KP_Separator -> Key_Comma
    (0xffad, 0x2d), // KP_Subtract -> Key_Minus
    (0xffae, 0x2e), // KP_Decimal -> Key_Period
    (0xffaf, 0x2f), // KP_Divide -> Key_Slash
    (0xff65, 0x1000123), // Undo -> Key_Undo
    (0xff66, 0x1000124), // Redo -> Key_Redo
    (0xff68, 0x1000122), // Find -> Key_Find
    (0xff69, 0x1020001), // Cancel -> Key_Cancel
    (0xfe03, 0x1001103), // ISO_Level3_Shift -> Key_AltGr
    (0xff20, 0x1001120), // Multi_key -> Key_Multi_key
    (0xff37, 0x1001137), // Codeinput -> Key_Codeinput
    (0xff3c, 0x100113c), // SingleCandidate -> Key_SingleCandidate
    (0xff3d, 0x100113d), // MultipleCandidate -> Key_MultipleCandidate
    (0xff3e, 0x100113e), // PreviousCandidate -> Key_PreviousCandidate
    (0xff7e, 0x100117e), // Mode_switch -> Key_Mode_switch
    (0xff7e, 0x100117e), // script_switch -> Key_Mode_switch
    (0xff21, 0x1001121), // Kanji -> Key_Kanji
    (0xff22, 0x1001122), // Muhenkan -> Key_Muhenkan
    (0xff23, 0x1001123), // Henkan_Mode -> Key_Henkan_Mode
    (0xff23, 0x1001123), // Henkan_Mode -> Key_Henkan
    (0xff23, 0x1001123), // Henkan -> Key_Henkan
    (0xff24, 0x1001124), // Romaji -> Key_Romaji
    (0xff25, 0x1001125), // Hiragana -> Key_Hiragana
    (0xff26, 0x1001126), // Katakana -> Key_Katakana
    (0xff27, 0x1001127), // Hiragana_Katakana -> Key_Hiragana_Katakana
    (0xff28, 0x1001128), // Zenkaku -> Key_Zenkaku
    (0xff29, 0x1001129), // Hankaku -> Key_Hankaku
    (0xff2a, 0x100112a), // Zenkaku_Hankaku -> Key_Zenkaku_Hankaku
    (0xff2b, 0x100112b), // Touroku -> Key_Touroku
    (0xff2c, 0x100112c), // Massyo -> Key_Massyo
    (0xff2d, 0x100112d), // Kana_Lock -> Key_Kana_Lock
    (0xff2e, 0x100112e), // Kana_Shift -> Key_Kana_Shift
    (0xff2f, 0x100112f), // Eisu_Shift -> Key_Eisu_Shift
    (0xff30, 0x1001130), // Eisu_toggle -> Key_Eisu_toggle
    (0xff37, 0x1001137), // Kanji_Bangou -> Key_Kanji_Bangou
    (0xff3d, 0x100113d), // Zen_Koho -> Key_Zen_Koho
    (0xff3e, 0x100113e), // Mae_Koho -> Key_Mae_Koho
    (0xff37, 0x1001137), // Kanji_Bangou -> Key_Codeinput
    (0xff3d, 0x100113d), // Zen_Koho -> Key_MultipleCandidate
    (0xff3e, 0x100113e), // Mae_Koho -> Key_PreviousCandidate
    (0xff31, 0x1001131), // Hangul -> Key_Hangul
    (0xff32, 0x1001132), // Hangul_Start -> Key_Hangul_Start
    (0xff33, 0x1001133), // Hangul_End -> Key_Hangul_End
    (0xff34, 0x1001134), // Hangul_Hanja -> Key_Hangul_Hanja
    (0xff35, 0x1001135), // Hangul_Jamo -> Key_Hangul_Jamo
    (0xff36, 0x1001136), // Hangul_Romaja -> Key_Hangul_Romaja
    (0xff37, 0x1001137), // Hangul_Codeinput -> Key_Hangul_Codeinput
    (0xff37, 0x1001137), // Hangul_Codeinput -> Key_Codeinput
    (0xff38, 0x1001138), // Hangul_Jeonja -> Key_Hangul_Jeonja
    (0xff39, 0x1001139), // Hangul_Banja -> Key_Hangul_Banja
    (0xff3a, 0x100113a), // Hangul_PreHanja -> Key_Hangul_PreHanja
    (0xff3b, 0x100113b), // Hangul_PostHanja -> Key_Hangul_PostHanja
    (0xff3c, 0x100113c), // Hangul_SingleCandidate -> Key_Hangul_SingleCandidate
    (0xff3d, 0x100113d), // Hangul_MultipleCandidate -> Key_Hangul_MultipleCandidate
    (0xff3e, 0x100113e), // Hangul_PreviousCandidate -> Key_Hangul_PreviousCandidate
    (0xff3c, 0x100113c), // Hangul_SingleCandidate -> Key_SingleCandidate
    (0xff3d, 0x100113d), // Hangul_MultipleCandidate -> Key_MultipleCandidate
    (0xff3e, 0x100113e), // Hangul_PreviousCandidate -> Key_PreviousCandidate
    (0xff3f, 0x100113f), // Hangul_Special -> Key_Hangul_Special
    (0xff7e, 0x100117e), // Hangul_switch -> Key_Hangul_switch
    (0xff7e, 0x100117e), // Hangul_switch -> Key_Mode_switch
    (0xfe50, 0x1001250), // dead_grave -> Key_Dead_Grave
    (0xfe51, 0x1001251), // dead_acute -> Key_Dead_Acute
    (0xfe52, 0x1001252), // dead_circumflex -> Key_Dead_Circumflex
    (0xfe53, 0x1001253), // dead_tilde -> Key_Dead_Tilde
    (0xfe54, 0x1001254), // dead_macron -> Key_Dead_Macron
    (0xfe55, 0x1001255), // dead_breve -> Key_Dead_Breve
    (0xfe56, 0x1001256), // dead_abovedot -> Key_Dead_Abovedot
    (0xfe57, 0x1001257), // dead_diaeresis -> Key_Dead_Diaeresis
    (0xfe58, 0x1001258), // dead_abovering -> Key_Dead_Abovering
    (0xfe59, 0x1001259), // dead_doubleacute -> Key_Dead_Doubleacute
    (0xfe5a, 0x100125a), // dead_caron -> Key_Dead_Caron
    (0xfe5b, 0x100125b), // dead_cedilla -> Key_Dead_Cedilla
    (0xfe5c, 0x100125c), // dead_ogonek -> Key_Dead_Ogonek
    (0xfe5d, 0x100125d), // dead_iota -> Key_Dead_Iota
    (0xfe5e, 0x100125e), // dead_voiced_sound -> Key_Dead_Voiced_Sound
    (0xfe5f, 0x100125f), // dead_semivoiced_sound -> Key_Dead_Semivoiced_Sound
    (0xfe60, 0x1001260), // dead_belowdot -> Key_Dead_Belowdot
    (0xfe61, 0x1001261), // dead_hook -> Key_Dead_Hook
    (0xfe62, 0x1001262), // dead_horn -> Key_Dead_Horn
    (0xfe63, 0x1001263), // dead_stroke -> Key_Dead_Stroke
    (0xfe64, 0x1001264), // dead_abovecomma -> Key_Dead_Abovecomma
    (0xfe65, 0x1001265), // dead_abovereversedcomma -> Key_Dead_Abovereversedcomma
    (0xfe66, 0x1001266), // dead_doublegrave -> Key_Dead_Doublegrave
    (0xfe67, 0x1001267), // dead_belowring -> Key_Dead_Belowring
    (0xfe68, 0x1001268), // dead_belowmacron -> Key_Dead_Belowmacron
    (0xfe69, 0x1001269), // dead_belowcircumflex -> Key_Dead_Belowcircumflex
    (0xfe6a, 0x100126a), // dead_belowtilde -> Key_Dead_Belowtilde
    (0xfe6b, 0x100126b), // dead_belowbreve -> Key_Dead_Belowbreve
    (0xfe6c, 0x100126c), // dead_belowdiaeresis -> Key_Dead_Belowdiaeresis
    (0xfe6d, 0x100126d), // dead_invertedbreve -> Key_Dead_Invertedbreve
    (0xfe6e, 0x100126e), // dead_belowcomma -> Key_Dead_Belowcomma
    (0xfe6f, 0x100126f), // dead_currency -> Key_Dead_Currency
    (0xfe80, 0x1001280), // dead_a -> Key_Dead_a
    (0xfe81, 0x1001281), // dead_A -> Key_Dead_A
    (0xfe82, 0x1001282), // dead_e -> Key_Dead_e
    (0xfe83, 0x1001283), // dead_E -> Key_Dead_E
    (0xfe84, 0x1001284), // dead_i -> Key_Dead_i
    (0xfe85, 0x1001285), // dead_I -> Key_Dead_I
    (0xfe86, 0x1001286), // dead_o -> Key_Dead_o
    (0xfe87, 0x1001287), // dead_O -> Key_Dead_O
    (0xfe88, 0x1001288), // dead_u -> Key_Dead_u
    (0xfe89, 0x1001289), // dead_U -> Key_Dead_U
    (0xfe8a, 0x100128a), // dead_small_schwa -> Key_Dead_Small_Schwa
    (0xfe8b, 0x100128b), // dead_capital_schwa -> Key_Dead_Capital_Schwa
    (0xfe8c, 0x100128c), // dead_greek -> Key_Dead_Greek
    (0xfe90, 0x1001290), // dead_lowline -> Key_Dead_Lowline
    (0xfe91, 0x1001291), // dead_aboveverticalline -> Key_Dead_Aboveverticalline
    (0xfe92, 0x1001292), // dead_belowverticalline -> Key_Dead_Belowverticalline
    (0xfe93, 0x1001293), // dead_longsolidusoverlay -> Key_Dead_Longsolidusoverlay
    (0x1008ff26, 0x1000061), // XF86Back -> Key_Back
    (0x1008ff27, 0x1000062), // XF86Forward -> Key_Forward
    (0x1008ff28, 0x1000063), // XF86Stop -> Key_Stop
    (0x1008ff29, 0x1000064), // XF86Refresh -> Key_Refresh
    (0x1008ff30, 0x1000091), // XF86Favorites -> Key_Favorites
    (0x1008ff32, 0x10000a1), // XF86AudioMedia -> Key_LaunchMedia
    (0x1008ff38, 0x1000094), // XF86OpenURL -> Key_OpenUrl
    (0x1008ff18, 0x1000090), // XF86HomePage -> Key_HomePage
    (0x1008ff1b, 0x1000092), // XF86Search -> Key_Search
    (0x1008ff11, 0x1000070), // XF86AudioLowerVolume -> Key_VolumeDown
    (0x1008ff12, 0x1000071), // XF86AudioMute -> Key_VolumeMute
    (0x1008ff13, 0x1000072), // XF86AudioRaiseVolume -> Key_VolumeUp
    (0x1008ff14, 0x1000080), // XF86AudioPlay -> Key_MediaPlay
    (0x1008ff15, 0x1000081), // XF86AudioStop -> Key_MediaStop
    (0x1008ff16, 0x1000082), // XF86AudioPrev -> Key_MediaPrevious
    (0x1008ff17, 0x1000083), // XF86AudioNext -> Key_MediaNext
    (0x1008ff1c, 0x1000084), // XF86AudioRecord -> Key_MediaRecord
    (0x1008ff31, 0x1000085), // XF86AudioPause -> Key_MediaPause
    (0x1008ff19, 0x10000a0), // XF86Mail -> Key_LaunchMail
    (0x1008ff33, 0x10000a1), // XF86MyComputer -> Key_LaunchMedia
    (0x1008ff1e, 0x10000bc), // XF86Memo -> Key_Memo
    (0x1008ff1f, 0x10000cc), // XF86ToDoList -> Key_ToDoList
    (0x1008ff20, 0x10000e4), // XF86Calendar -> Key_Calendar
    (0x1008ff21, 0x100010b), // XF86PowerDown -> Key_PowerDown
    (0x1008ff22, 0x100010d), // XF86ContrastAdjust -> Key_ContrastAdjust
    (0x1008ff10, 0x1000093), // XF86Standby -> Key_Standby
    (0x1008ff02, 0x10000b2), // XF86MonBrightnessUp -> Key_MonBrightnessUp
    (0x1008ff03, 0x10000b3), // XF86MonBrightnessDown -> Key_MonBrightnessDown
    (0x1008ff04, 0x10000b4), // XF86KbdLightOnOff -> Key_KeyboardLightOnOff
    (0x1008ff05, 0x10000b5), // XF86KbdBrightnessUp -> Key_KeyboardBrightnessUp
    (0x1008ff06, 0x10000b6), // XF86KbdBrightnessDown -> Key_KeyboardBrightnessDown
    (0x1008ff2a, 0x10000b7), // XF86PowerOff -> Key_PowerOff
    (0x1008ff2b, 0x10000b8), // XF86WakeUp -> Key_WakeUp
    (0x1008ff2c, 0x10000b9), // XF86Eject -> Key_Eject
    (0x1008ff2d, 0x10000ba), // XF86ScreenSaver -> Key_ScreenSaver
    (0x1008ff2e, 0x10000bb), // XF86WWW -> Key_WWW
    (0x1008ff2f, 0x1020004), // XF86Sleep -> Key_Sleep
    (0x1008ff35, 0x10000bd), // XF86LightBulb -> Key_LightBulb
    (0x1008ff36, 0x10000be), // XF86Shop -> Key_Shop
    (0x1008ff37, 0x10000bf), // XF86History -> Key_History
    (0x1008ff39, 0x10000c0), // XF86AddFavorite -> Key_AddFavorite
    (0x1008ff3a, 0x10000c1), // XF86HotLinks -> Key_HotLinks
    (0x1008ff3b, 0x10000c2), // XF86BrightnessAdjust -> Key_BrightnessAdjust
    (0x1008ff3c, 0x10000c3), // XF86Finance -> Key_Finance
    (0x1008ff3d, 0x10000c4), // XF86Community -> Key_Community
    (0x1008ff3e, 0x10000c5), // XF86AudioRewind -> Key_AudioRewind
    (0x1008ff3f, 0x10000c6), // XF86BackForward -> Key_BackForward
    (0x1008ff50, 0x10000c7), // XF86ApplicationLeft -> Key_ApplicationLeft
    (0x1008ff51, 0x10000c8), // XF86ApplicationRight -> Key_ApplicationRight
    (0x1008ff52, 0x10000c9), // XF86Book -> Key_Book
    (0x1008ff53, 0x10000ca), // XF86CD -> Key_CD
    (0x1008ff54, 0x10000cb), // XF86Calculater -> Key_Calculator
    (0x1008ff1d, 0x10000cb), // XF86Calculator -> Key_Calculator
    (0x1008ff55, 0x100000b), // XF86Clear -> Key_Clear
    (0x1008fe21, 0x10000cd), // XF86ClearGrab -> Key_ClearGrab
    (0x1008ff56, 0x10000ce), // XF86Close -> Key_Close
    (0x1008ff57, 0x10000cf), // XF86Copy -> Key_Copy
    (0x1008ff58, 0x10000d0), // XF86Cut -> Key_Cut
    (0x1008ff59, 0x10000d1), // XF86Display -> Key_Display
    (0x1008ff5a, 0x10000d2), // XF86DOS -> Key_DOS
    (0x1008ff5b, 0x10000d3), // XF86Documents -> Key_Documents
    (0x1008ff5c, 0x10000d4), // XF86Excel -> Key_Excel
    (0x1008ff5d, 0x10000d5), // XF86Explorer -> Key_Explorer
    (0x1008ff5e, 0x10000d6), // XF86Game -> Key_Game
    (0x1008ff5f, 0x10000d7), // XF86Go -> Key_Go
    (0x1008ff60, 0x10000d8), // XF86iTouch -> Key_iTouch
    (0x1008ff61, 0x10000d9), // XF86LogOff -> Key_LogOff
    (0x1008ff62, 0x10000da), // XF86Market -> Key_Market
    (0x1008ff63, 0x10000db), // XF86Meeting -> Key_Meeting
    (0x1008ff65, 0x10000dc), // XF86MenuKB -> Key_MenuKB
    (0x1008ff66, 0x10000dd), // XF86MenuPB -> Key_MenuPB
    (0x1008ff67, 0x10000de), // XF86MySites -> Key_MySites
    (0x1008ff68, 0x1000120), // XF86New -> Key_New
    (0x1008ff69, 0x10000df), // XF86News -> Key_News
    (0x1008ff6a, 0x10000e0), // XF86OfficeHome -> Key_OfficeHome
    (0x1008ff6b, 0x1000121), // XF86Open -> Key_Open
    (0x1008ff6c, 0x10000e1), // XF86Option -> Key_Option
    (0x1008ff6d, 0x10000e2), // XF86Paste -> Key_Paste
    (0x1008ff6e, 0x10000e3), // XF86Phone -> Key_Phone
    (0x1008ff72, 0x10000e5), // XF86Reply -> Key_Reply
    (0x1008ff73, 0x10000e6), // XF86Reload -> Key_Reload
    (0x1008ff74, 0x10000e7), // XF86RotateWindows -> Key_RotateWindows
    (0x1008ff75, 0x10000e8), // XF86RotationPB -> Key_RotationPB
    (0x1008ff76, 0x10000e9), // XF86RotationKB -> Key_RotationKB
    (0x1008ff77, 0x10000ea), // XF86Save -> Key_Save
    (0x1008ff7b, 0x10000eb), // XF86Send -> Key_Send
    (0x1008ff7c, 0x10000ec), // XF86Spell -> Key_Spell
    (0x1008ff7d, 0x10000ed), // XF86SplitScreen -> Key_SplitScreen
    (0x1008ff7e, 0x10000ee), // XF86Support -> Key_Support
    (0x1008ff7f, 0x10000ef), // XF86TaskPane -> Key_TaskPane
    (0x1008ff80, 0x10000f0), // XF86Terminal -> Key_Terminal
    (0x1008ff81, 0x10000f1), // XF86Tools -> Key_Tools
    (0x1008ff82, 0x10000f2), // XF86Travel -> Key_Travel
    (0x1008ff87, 0x10000f3), // XF86Video -> Key_Video
    (0x1008ff89, 0x10000f4), // XF86Word -> Key_Word
    (0x1008ff8a, 0x10000f5), // XF86Xfer -> Key_Xfer
    (0x1008ff8b, 0x10000f6), // XF86ZoomIn -> Key_ZoomIn
    (0x1008ff8c, 0x10000f7), // XF86ZoomOut -> Key_ZoomOut
    (0x1008ff8d, 0x10000f8), // XF86Away -> Key_Away
    (0x1008ff8e, 0x10000f9), // XF86Messenger -> Key_Messenger
    (0x1008ff8f, 0x10000fa), // XF86WebCam -> Key_WebCam
    (0x1008ff90, 0x10000fb), // XF86MailForward -> Key_MailForward
    (0x1008ff91, 0x10000fc), // XF86Pictures -> Key_Pictures
    (0x1008ff92, 0x10000fd), // XF86Music -> Key_Music
    (0x1008ff93, 0x10000fe), // XF86Battery -> Key_Battery
    (0x1008ff94, 0x10000ff), // XF86Bluetooth -> Key_Bluetooth
    (0x1008ff95, 0x1000100), // XF86WLAN -> Key_WLAN
    (0x1008ff96, 0x1000101), // XF86UWB -> Key_UWB
    (0x1008ff97, 0x1000102), // XF86AudioForward -> Key_AudioForward
    (0x1008ff98, 0x1000103), // XF86AudioRepeat -> Key_AudioRepeat
    (0x1008ff99, 0x1000104), // XF86AudioRandomPlay -> Key_AudioRandomPlay
    (0x1008ff9a, 0x1000105), // XF86Subtitle -> Key_Subtitle
    (0x1008ff9b, 0x1000106), // XF86AudioCycleTrack -> Key_AudioCycleTrack
    (0x1008ff9f, 0x1000107), // XF86Time -> Key_Time
    (0x1008ffa0, 0x1010000), // XF86Select -> Key_Select
    (0x1008ffa1, 0x1000109), // XF86View -> Key_View
    (0x1008ffa2, 0x100010a), // XF86TopMenu -> Key_TopMenu
    (0x1008ffa3, 0x1000114), // XF86Red -> Key_Red
    (0x1008ffa4, 0x1000115), // XF86Green -> Key_Green
    (0x1008ffa5, 0x1000116), // XF86Yellow -> Key_Yellow
    (0x1008ffa6, 0x1000117), // XF86Blue -> Key_Blue
    (0x1008ff94, 0x10000ff), // XF86Bluetooth -> Key_Bluetooth
    (0x1008ffa7, 0x100010c), // XF86Suspend -> Key_Suspend
    (0x1008ffa8, 0x1000108), // XF86Hibernate -> Key_Hibernate
    (0x1008ffa9, 0x1000110), // XF86TouchpadToggle -> Key_TouchpadToggle
    (0x1008ffb0, 0x1000111), // XF86TouchpadOn -> Key_TouchpadOn
    (0x1008ffb1, 0x1000112), // XF86TouchpadOff -> Key_TouchpadOff
    (0x1008ffb2, 0x1000113), // XF86AudioMicMute -> Key_MicMute
    (0x1008ffb3, 0x100011f), // XF86Keyboard -> Key_Keyboard
    (0x1008ff40, 0x10000a2), // XF86Launch0 -> Key_Launch0
    (0x1008ff41, 0x10000a3), // XF86Launch1 -> Key_Launch1
    (0x1008ff42, 0x10000a4), // XF86Launch2 -> Key_Launch2
    (0x1008ff43, 0x10000a5), // XF86Launch3 -> Key_Launch3
    (0x1008ff44, 0x10000a6), // XF86Launch4 -> Key_Launch4
    (0x1008ff45, 0x10000a7), // XF86Launch5 -> Key_Launch5
    (0x1008ff46, 0x10000a8), // XF86Launch6 -> Key_Launch6
    (0x1008ff47, 0x10000a9), // XF86Launch7 -> Key_Launch7
    (0x1008ff48, 0x10000aa), // XF86Launch8 -> Key_Launch8
    (0x1008ff49, 0x10000ab), // XF86Launch9 -> Key_Launch9
    (0x1008ff4a, 0x10000ac), // XF86LaunchA -> Key_LaunchA
    (0x1008ff4b, 0x10000ad), // XF86LaunchB -> Key_LaunchB
    (0x1008ff4c, 0x10000ae), // XF86LaunchC -> Key_LaunchC
    (0x1008ff4d, 0x10000af), // XF86LaunchD -> Key_LaunchD
    (0x1008ff4e, 0x10000b0), // XF86LaunchE -> Key_LaunchE
    (0x1008ff4f, 0x10000b1), // XF86LaunchF -> Key_LaunchF
];

/// (Qt::Key, portable name) as QKeySequence::PortableText writes them.
pub static QT_KEY_NAMES: &[(u32, &str)] = &[
    (0x20, "Space"), // Key_Space
    (0x1000000, "Esc"), // Key_Escape
    (0x1000001, "Tab"), // Key_Tab
    (0x1000002, "Backtab"), // Key_Backtab
    (0x1000003, "Backspace"), // Key_Backspace
    (0x1000004, "Return"), // Key_Return
    (0x1000005, "Enter"), // Key_Enter
    (0x1000006, "Ins"), // Key_Insert
    (0x1000007, "Del"), // Key_Delete
    (0x1000008, "Pause"), // Key_Pause
    (0x1000009, "Print"), // Key_Print
    (0x100000a, "SysReq"), // Key_SysReq
    (0x1000010, "Home"), // Key_Home
    (0x1000011, "End"), // Key_End
    (0x1000012, "Left"), // Key_Left
    (0x1000013, "Up"), // Key_Up
    (0x1000014, "Right"), // Key_Right
    (0x1000015, "Down"), // Key_Down
    (0x1000016, "PgUp"), // Key_PageUp
    (0x1000017, "PgDown"), // Key_PageDown
    (0x1000024, "CapsLock"), // Key_CapsLock
    (0x1000025, "NumLock"), // Key_NumLock
    (0x1000026, "ScrollLock"), // Key_ScrollLock
    (0x1000055, "Menu"), // Key_Menu
    (0x1000058, "Help"), // Key_Help
    (0x1000061, "Back"), // Key_Back
    (0x1000062, "Forward"), // Key_Forward
    (0x1000063, "Stop"), // Key_Stop
    (0x1000064, "Refresh"), // Key_Refresh
    (0x1000070, "Volume Down"), // Key_VolumeDown
    (0x1000071, "Volume Mute"), // Key_VolumeMute
    (0x1000072, "Volume Up"), // Key_VolumeUp
    (0x1000073, "Bass Boost"), // Key_BassBoost
    (0x1000074, "Bass Up"), // Key_BassUp
    (0x1000075, "Bass Down"), // Key_BassDown
    (0x1000076, "Treble Up"), // Key_TrebleUp
    (0x1000077, "Treble Down"), // Key_TrebleDown
    (0x1000080, "Media Play"), // Key_MediaPlay
    (0x1000081, "Media Stop"), // Key_MediaStop
    (0x1000082, "Media Previous"), // Key_MediaPrevious
    (0x1000083, "Media Next"), // Key_MediaNext
    (0x1000084, "Media Record"), // Key_MediaRecord
    (0x1000085, "Media Pause"), // Key_MediaPause
    (0x1000086, "Toggle Media Play/Pause"), // Key_MediaTogglePlayPause
    (0x1000090, "Home Page"), // Key_HomePage
    (0x1000091, "Favorites"), // Key_Favorites
    (0x1000092, "Search"), // Key_Search
    (0x1000093, "Standby"), // Key_Standby
    (0x1000094, "Open URL"), // Key_OpenUrl
    (0x10000a0, "Launch Mail"), // Key_LaunchMail
    (0x10000a1, "Launch Media"), // Key_LaunchMedia
    (0x10000a2, "Launch (0)"), // Key_Launch0
    (0x10000a3, "Launch (1)"), // Key_Launch1
    (0x10000a4, "Launch (2)"), // Key_Launch2
    (0x10000a5, "Launch (3)"), // Key_Launch3
    (0x10000a6, "Launch (4)"), // Key_Launch4
    (0x10000a7, "Launch (5)"), // Key_Launch5
    (0x10000a8, "Launch (6)"), // Key_Launch6
    (0x10000a9, "Launch (7)"), // Key_Launch7
    (0x10000aa, "Launch (8)"), // Key_Launch8
    (0x10000ab, "Launch (9)"), // Key_Launch9
    (0x10000ac, "Launch (A)"), // Key_LaunchA
    (0x10000ad, "Launch (B)"), // Key_LaunchB
    (0x10000ae, "Launch (C)"), // Key_LaunchC
    (0x10000af, "Launch (D)"), // Key_LaunchD
    (0x10000b0, "Launch (E)"), // Key_LaunchE
    (0x10000b1, "Launch (F)"), // Key_LaunchF
    (0x100010e, "Launch (G)"), // Key_LaunchG
    (0x100010f, "Launch (H)"), // Key_LaunchH
    (0x10000b2, "Monitor Brightness Up"), // Key_MonBrightnessUp
    (0x10000b3, "Monitor Brightness Down"), // Key_MonBrightnessDown
    (0x10000b4, "Keyboard Light On/Off"), // Key_KeyboardLightOnOff
    (0x10000b5, "Keyboard Brightness Up"), // Key_KeyboardBrightnessUp
    (0x10000b6, "Keyboard Brightness Down"), // Key_KeyboardBrightnessDown
    (0x10000b7, "Power Off"), // Key_PowerOff
    (0x10000b8, "Wake Up"), // Key_WakeUp
    (0x10000b9, "Eject"), // Key_Eject
    (0x10000ba, "Screensaver"), // Key_ScreenSaver
    (0x10000bb, "WWW"), // Key_WWW
    (0x1020004, "Sleep"), // Key_Sleep
    (0x10000bd, "LightBulb"), // Key_LightBulb
    (0x10000be, "Shop"), // Key_Shop
    (0x10000bf, "History"), // Key_History
    (0x10000c0, "Add Favorite"), // Key_AddFavorite
    (0x10000c1, "Hot Links"), // Key_HotLinks
    (0x10000c2, "Adjust Brightness"), // Key_BrightnessAdjust
    (0x10000c3, "Finance"), // Key_Finance
    (0x10000c4, "Community"), // Key_Community
    (0x10000c5, "Media Rewind"), // Key_AudioRewind
    (0x10000c6, "Back Forward"), // Key_BackForward
    (0x10000c7, "Application Left"), // Key_ApplicationLeft
    (0x10000c8, "Application Right"), // Key_ApplicationRight
    (0x10000c9, "Book"), // Key_Book
    (0x10000ca, "CD"), // Key_CD
    (0x10000cb, "Calculator"), // Key_Calculator
    (0x10000e4, "Calendar"), // Key_Calendar
    (0x100000b, "Clear"), // Key_Clear
    (0x10000cd, "Clear Grab"), // Key_ClearGrab
    (0x10000ce, "Close"), // Key_Close
    (0x100010d, "Adjust contrast"), // Key_ContrastAdjust
    (0x10000cf, "Copy"), // Key_Copy
    (0x10000d0, "Cut"), // Key_Cut
    (0x10000d1, "Display"), // Key_Display
    (0x10000d2, "DOS"), // Key_DOS
    (0x10000d3, "Documents"), // Key_Documents
    (0x10000d4, "Spreadsheet"), // Key_Excel
    (0x10000d5, "Browser"), // Key_Explorer
    (0x10000d6, "Game"), // Key_Game
    (0x10000d7, "Go"), // Key_Go
    (0x10000d8, "iTouch"), // Key_iTouch
    (0x10000d9, "Logoff"), // Key_LogOff
    (0x10000da, "Market"), // Key_Market
    (0x10000db, "Meeting"), // Key_Meeting
    (0x10000bc, "Memo"), // Key_Memo
    (0x10000dc, "Keyboard Menu"), // Key_MenuKB
    (0x10000dd, "Menu PB"), // Key_MenuPB
    (0x10000de, "My Sites"), // Key_MySites
    (0x10000df, "News"), // Key_News
    (0x10000e0, "Home Office"), // Key_OfficeHome
    (0x10000e1, "Option"), // Key_Option
    (0x10000e2, "Paste"), // Key_Paste
    (0x10000e3, "Phone"), // Key_Phone
    (0x10000e5, "Reply"), // Key_Reply
    (0x10000e6, "Reload"), // Key_Reload
    (0x10000e7, "Rotate Windows"), // Key_RotateWindows
    (0x10000e8, "Rotation PB"), // Key_RotationPB
    (0x10000e9, "Rotation KB"), // Key_RotationKB
    (0x10000ea, "Save"), // Key_Save
    (0x10000eb, "Send"), // Key_Send
    (0x10000ec, "Spellchecker"), // Key_Spell
    (0x10000ed, "Split Screen"), // Key_SplitScreen
    (0x10000ee, "Support"), // Key_Support
    (0x10000ef, "Task Panel"), // Key_TaskPane
    (0x10000f0, "Terminal"), // Key_Terminal
    (0x10000cc, "To-do list"), // Key_ToDoList
    (0x10000f1, "Tools"), // Key_Tools
    (0x10000f2, "Travel"), // Key_Travel
    (0x10000f3, "Video"), // Key_Video
    (0x10000f4, "Word Processor"), // Key_Word
    (0x10000f5, "XFer"), // Key_Xfer
    (0x10000f6, "Zoom In"), // Key_ZoomIn
    (0x10000f7, "Zoom Out"), // Key_ZoomOut
    (0x10000f8, "Away"), // Key_Away
    (0x10000f9, "Messenger"), // Key_Messenger
    (0x10000fa, "WebCam"), // Key_WebCam
    (0x10000fb, "Mail Forward"), // Key_MailForward
    (0x10000fc, "Pictures"), // Key_Pictures
    (0x10000fd, "Music"), // Key_Music
    (0x10000fe, "Battery"), // Key_Battery
    (0x10000ff, "Bluetooth"), // Key_Bluetooth
    (0x1000100, "Wireless"), // Key_WLAN
    (0x1000101, "Ultra Wide Band"), // Key_UWB
    (0x1000102, "Media Fast Forward"), // Key_AudioForward
    (0x1000103, "Audio Repeat"), // Key_AudioRepeat
    (0x1000104, "Audio Random Play"), // Key_AudioRandomPlay
    (0x1000105, "Subtitle"), // Key_Subtitle
    (0x1000106, "Audio Cycle Track"), // Key_AudioCycleTrack
    (0x1000107, "Time"), // Key_Time
    (0x1000108, "Hibernate"), // Key_Hibernate
    (0x1000109, "View"), // Key_View
    (0x100010a, "Top Menu"), // Key_TopMenu
    (0x100010b, "Power Down"), // Key_PowerDown
    (0x100010c, "Suspend"), // Key_Suspend
    (0x1000113, "Microphone Mute"), // Key_MicMute
    (0x1000114, "Red"), // Key_Red
    (0x1000115, "Green"), // Key_Green
    (0x1000116, "Yellow"), // Key_Yellow
    (0x1000117, "Blue"), // Key_Blue
    (0x1000118, "Channel Up"), // Key_ChannelUp
    (0x1000119, "Channel Down"), // Key_ChannelDown
    (0x100011a, "Guide"), // Key_Guide
    (0x100011b, "Info"), // Key_Info
    (0x100011c, "Settings"), // Key_Settings
    (0x100011d, "Microphone Volume Up"), // Key_MicVolumeUp
    (0x100011e, "Microphone Volume Down"), // Key_MicVolumeDown
    (0x100011f, "Keyboard"), // Key_Keyboard
    (0x1000120, "New"), // Key_New
    (0x1000121, "Open"), // Key_Open
    (0x1000122, "Find"), // Key_Find
    (0x1000123, "Undo"), // Key_Undo
    (0x1000124, "Redo"), // Key_Redo
    (0x1000009, "Print Screen"), // Key_Print
    (0x1000016, "Page Up"), // Key_PageUp
    (0x1000017, "Page Down"), // Key_PageDown
    (0x1000024, "Caps Lock"), // Key_CapsLock
    (0x1000025, "Num Lock"), // Key_NumLock
    (0x1000025, "Number Lock"), // Key_NumLock
    (0x1000026, "Scroll Lock"), // Key_ScrollLock
    (0x1000006, "Insert"), // Key_Insert
    (0x1000007, "Delete"), // Key_Delete
    (0x1000000, "Escape"), // Key_Escape
    (0x100000a, "System Request"), // Key_SysReq
    (0x1010000, "Select"), // Key_Select
    (0x1010001, "Yes"), // Key_Yes
    (0x1010002, "No"), // Key_No
    (0x1100000, "Context1"), // Key_Context1
    (0x1100001, "Context2"), // Key_Context2
    (0x1100002, "Context3"), // Key_Context3
    (0x1100003, "Context4"), // Key_Context4
    (0x1100004, "Call"), // Key_Call
    (0x1100005, "Hangup"), // Key_Hangup
    (0x1100007, "Toggle Call/Hangup"), // Key_ToggleCallHangup
    (0x1100006, "Flip"), // Key_Flip
    (0x1100008, "Voice Dial"), // Key_VoiceDial
    (0x1100009, "Last Number Redial"), // Key_LastNumberRedial
    (0x1100020, "Camera Shutter"), // Key_Camera
    (0x1100021, "Camera Focus"), // Key_CameraFocus
    (0x1001121, "Kanji"), // Key_Kanji
    (0x1001122, "Muhenkan"), // Key_Muhenkan
    (0x1001123, "Henkan"), // Key_Henkan
    (0x1001124, "Romaji"), // Key_Romaji
    (0x1001125, "Hiragana"), // Key_Hiragana
    (0x1001126, "Katakana"), // Key_Katakana
    (0x1001127, "Hiragana Katakana"), // Key_Hiragana_Katakana
    (0x1001128, "Zenkaku"), // Key_Zenkaku
    (0x1001129, "Hankaku"), // Key_Hankaku
    (0x100112a, "Zenkaku Hankaku"), // Key_Zenkaku_Hankaku
    (0x100112b, "Touroku"), // Key_Touroku
    (0x100112c, "Massyo"), // Key_Massyo
    (0x100112d, "Kana Lock"), // Key_Kana_Lock
    (0x100112e, "Kana Shift"), // Key_Kana_Shift
    (0x100112f, "Eisu Shift"), // Key_Eisu_Shift
    (0x1001130, "Eisu toggle"), // Key_Eisu_toggle
    (0x1001137, "Code input"), // Key_Codeinput
    (0x100113d, "Multiple Candidate"), // Key_MultipleCandidate
    (0x100113e, "Previous Candidate"), // Key_PreviousCandidate
    (0x1001131, "Hangul"), // Key_Hangul
    (0x1001132, "Hangul Start"), // Key_Hangul_Start
    (0x1001133, "Hangul End"), // Key_Hangul_End
    (0x1001134, "Hangul Hanja"), // Key_Hangul_Hanja
    (0x1001135, "Hangul Jamo"), // Key_Hangul_Jamo
    (0x1001136, "Hangul Romaja"), // Key_Hangul_Romaja
    (0x1001138, "Hangul Jeonja"), // Key_Hangul_Jeonja
    (0x1001139, "Hangul Banja"), // Key_Hangul_Banja
    (0x100113a, "Hangul PreHanja"), // Key_Hangul_PreHanja
    (0x100113b, "Hangul PostHanja"), // Key_Hangul_PostHanja
    (0x100113f, "Hangul Special"), // Key_Hangul_Special
    (0x1020001, "Cancel"), // Key_Cancel
    (0x1020002, "Printer"), // Key_Printer
    (0x1020003, "Execute"), // Key_Execute
    (0x1020005, "Play"), // Key_Play
    (0x1020006, "Zoom"), // Key_Zoom
    (0x102000a, "Exit"), // Key_Exit
    (0x1000110, "Touchpad Toggle"), // Key_TouchpadToggle
    (0x1000111, "Touchpad On"), // Key_TouchpadOn
    (0x1000112, "Touchpad Off"), // Key_TouchpadOff
    (0x1000020, "Shift"), // Key_Shift
    (0x1000021, "Control"), // Key_Control
    (0x1000023, "Alt"), // Key_Alt
    (0x1000022, "Meta"), // Key_Meta
];
