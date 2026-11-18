//! uinput macro playback — pure-Rust port of `k100play.py::Player`.
//!
//! A virtual input device named `icue-nexus-display-macro` (all key codes < 0x300
//! plus the REL axes k100play declared) replays `MacroEvent` lists with the
//! recorded inter-event delays scaled by `speed` (0.25..=4).  Repeat mode
//! loops with a 0.1 s / speed gap between iterations.
//!
//! Every playback run gets a sequence id echoed back through
//! `Internal::Playing` so the manager can ignore stop notifications from
//! runs it already replaced, and a drop guard releases every key the run
//! still holds — on stop, on drop and while unwinding a panic (never leave
//! a modifier stuck down).
//!
//! Name/code lookups use the ported python-evdev tables below; keys and
//! relative axes keep separate maps because their code numbers collide
//! (`KEY_7 == 8 == REL_WHEEL`), exactly like Python's `kind_names()`.

use super::manager::Internal;
use crate::core::events::{CoreEvent, LogLevel};
use crate::core::types::{MacroEvent, MacroEventKind};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

// ─── evdev code ↔ name tables (ported verbatim from python-evdev's
// `ecodes` as consumed by k100play.py's `_name_table`) ─────────────
//
// One canonical `(code, name)` list per event type, sorted by code,
// serving both directions (code→name via binary search, name→code via
// linear scan).  python-evdev resolves alias symbols by first-declaration
// order, baked in here (code 153 → KEY_DIRECTION; KEY_ROTATE_DISPLAY is
// absent, exactly like Python's reverse map).  Code numbers collide across
// event types (KEY_7 == 8 == REL_WHEEL), so keys and relative axes never
// share a namespace.

pub(crate) const KEY_TABLE: &[(u16, &str)] = &[
    (0, "KEY_RESERVED"), (1, "KEY_ESC"), (2, "KEY_1"), (3, "KEY_2"),
    (4, "KEY_3"), (5, "KEY_4"), (6, "KEY_5"), (7, "KEY_6"),
    (8, "KEY_7"), (9, "KEY_8"), (10, "KEY_9"), (11, "KEY_0"),
    (12, "KEY_MINUS"), (13, "KEY_EQUAL"), (14, "KEY_BACKSPACE"), (15, "KEY_TAB"),
    (16, "KEY_Q"), (17, "KEY_W"), (18, "KEY_E"), (19, "KEY_R"),
    (20, "KEY_T"), (21, "KEY_Y"), (22, "KEY_U"), (23, "KEY_I"),
    (24, "KEY_O"), (25, "KEY_P"), (26, "KEY_LEFTBRACE"), (27, "KEY_RIGHTBRACE"),
    (28, "KEY_ENTER"), (29, "KEY_LEFTCTRL"), (30, "KEY_A"), (31, "KEY_S"),
    (32, "KEY_D"), (33, "KEY_F"), (34, "KEY_G"), (35, "KEY_H"),
    (36, "KEY_J"), (37, "KEY_K"), (38, "KEY_L"), (39, "KEY_SEMICOLON"),
    (40, "KEY_APOSTROPHE"), (41, "KEY_GRAVE"), (42, "KEY_LEFTSHIFT"), (43, "KEY_BACKSLASH"),
    (44, "KEY_Z"), (45, "KEY_X"), (46, "KEY_C"), (47, "KEY_V"),
    (48, "KEY_B"), (49, "KEY_N"), (50, "KEY_M"), (51, "KEY_COMMA"),
    (52, "KEY_DOT"), (53, "KEY_SLASH"), (54, "KEY_RIGHTSHIFT"), (55, "KEY_KPASTERISK"),
    (56, "KEY_LEFTALT"), (57, "KEY_SPACE"), (58, "KEY_CAPSLOCK"), (59, "KEY_F1"),
    (60, "KEY_F2"), (61, "KEY_F3"), (62, "KEY_F4"), (63, "KEY_F5"),
    (64, "KEY_F6"), (65, "KEY_F7"), (66, "KEY_F8"), (67, "KEY_F9"),
    (68, "KEY_F10"), (69, "KEY_NUMLOCK"), (70, "KEY_SCROLLLOCK"), (71, "KEY_KP7"),
    (72, "KEY_KP8"), (73, "KEY_KP9"), (74, "KEY_KPMINUS"), (75, "KEY_KP4"),
    (76, "KEY_KP5"), (77, "KEY_KP6"), (78, "KEY_KPPLUS"), (79, "KEY_KP1"),
    (80, "KEY_KP2"), (81, "KEY_KP3"), (82, "KEY_KP0"), (83, "KEY_KPDOT"),
    (85, "KEY_ZENKAKUHANKAKU"), (86, "KEY_102ND"), (87, "KEY_F11"), (88, "KEY_F12"),
    (89, "KEY_RO"), (90, "KEY_KATAKANA"), (91, "KEY_HIRAGANA"), (92, "KEY_HENKAN"),
    (93, "KEY_KATAKANAHIRAGANA"), (94, "KEY_MUHENKAN"), (95, "KEY_KPJPCOMMA"), (96, "KEY_KPENTER"),
    (97, "KEY_RIGHTCTRL"), (98, "KEY_KPSLASH"), (99, "KEY_SYSRQ"), (100, "KEY_RIGHTALT"),
    (101, "KEY_LINEFEED"), (102, "KEY_HOME"), (103, "KEY_UP"), (104, "KEY_PAGEUP"),
    (105, "KEY_LEFT"), (106, "KEY_RIGHT"), (107, "KEY_END"), (108, "KEY_DOWN"),
    (109, "KEY_PAGEDOWN"), (110, "KEY_INSERT"), (111, "KEY_DELETE"), (112, "KEY_MACRO"),
    (113, "KEY_MIN_INTERESTING"), (114, "KEY_VOLUMEDOWN"), (115, "KEY_VOLUMEUP"), (116, "KEY_POWER"),
    (117, "KEY_KPEQUAL"), (118, "KEY_KPPLUSMINUS"), (119, "KEY_PAUSE"), (120, "KEY_SCALE"),
    (121, "KEY_KPCOMMA"), (122, "KEY_HANGEUL"), (123, "KEY_HANJA"), (124, "KEY_YEN"),
    (125, "KEY_LEFTMETA"), (126, "KEY_RIGHTMETA"), (127, "KEY_COMPOSE"), (128, "KEY_STOP"),
    (129, "KEY_AGAIN"), (130, "KEY_PROPS"), (131, "KEY_UNDO"), (132, "KEY_FRONT"),
    (133, "KEY_COPY"), (134, "KEY_OPEN"), (135, "KEY_PASTE"), (136, "KEY_FIND"),
    (137, "KEY_CUT"), (138, "KEY_HELP"), (139, "KEY_MENU"), (140, "KEY_CALC"),
    (141, "KEY_SETUP"), (142, "KEY_SLEEP"), (143, "KEY_WAKEUP"), (144, "KEY_FILE"),
    (145, "KEY_SENDFILE"), (146, "KEY_DELETEFILE"), (147, "KEY_XFER"), (148, "KEY_PROG1"),
    (149, "KEY_PROG2"), (150, "KEY_WWW"), (151, "KEY_MSDOS"), (152, "KEY_COFFEE"),
    (153, "KEY_DIRECTION"), (154, "KEY_CYCLEWINDOWS"), (155, "KEY_MAIL"), (156, "KEY_BOOKMARKS"),
    (157, "KEY_COMPUTER"), (158, "KEY_BACK"), (159, "KEY_FORWARD"), (160, "KEY_CLOSECD"),
    (161, "KEY_EJECTCD"), (162, "KEY_EJECTCLOSECD"), (163, "KEY_NEXTSONG"), (164, "KEY_PLAYPAUSE"),
    (165, "KEY_PREVIOUSSONG"), (166, "KEY_STOPCD"), (167, "KEY_RECORD"), (168, "KEY_REWIND"),
    (169, "KEY_PHONE"), (170, "KEY_ISO"), (171, "KEY_CONFIG"), (172, "KEY_HOMEPAGE"),
    (173, "KEY_REFRESH"), (174, "KEY_EXIT"), (175, "KEY_MOVE"), (176, "KEY_EDIT"),
    (177, "KEY_SCROLLUP"), (178, "KEY_SCROLLDOWN"), (179, "KEY_KPLEFTPAREN"), (180, "KEY_KPRIGHTPAREN"),
    (181, "KEY_NEW"), (182, "KEY_REDO"), (183, "KEY_F13"), (184, "KEY_F14"),
    (185, "KEY_F15"), (186, "KEY_F16"), (187, "KEY_F17"), (188, "KEY_F18"),
    (189, "KEY_F19"), (190, "KEY_F20"), (191, "KEY_F21"), (192, "KEY_F22"),
    (193, "KEY_F23"), (194, "KEY_F24"), (200, "KEY_PLAYCD"), (201, "KEY_PAUSECD"),
    (202, "KEY_PROG3"), (203, "KEY_PROG4"), (204, "KEY_ALL_APPLICATIONS"), (205, "KEY_SUSPEND"),
    (206, "KEY_CLOSE"), (207, "KEY_PLAY"), (208, "KEY_FASTFORWARD"), (209, "KEY_BASSBOOST"),
    (210, "KEY_PRINT"), (211, "KEY_HP"), (212, "KEY_CAMERA"), (213, "KEY_SOUND"),
    (214, "KEY_QUESTION"), (215, "KEY_EMAIL"), (216, "KEY_CHAT"), (217, "KEY_SEARCH"),
    (218, "KEY_CONNECT"), (219, "KEY_FINANCE"), (220, "KEY_SPORT"), (221, "KEY_SHOP"),
    (222, "KEY_ALTERASE"), (223, "KEY_CANCEL"), (224, "KEY_BRIGHTNESSDOWN"), (225, "KEY_BRIGHTNESSUP"),
    (226, "KEY_MEDIA"), (227, "KEY_SWITCHVIDEOMODE"), (228, "KEY_KBDILLUMTOGGLE"), (229, "KEY_KBDILLUMDOWN"),
    (230, "KEY_KBDILLUMUP"), (231, "KEY_SEND"), (232, "KEY_REPLY"), (233, "KEY_FORWARDMAIL"),
    (234, "KEY_SAVE"), (235, "KEY_DOCUMENTS"), (236, "KEY_BATTERY"), (237, "KEY_BLUETOOTH"),
    (238, "KEY_WLAN"), (239, "KEY_UWB"), (240, "KEY_UNKNOWN"), (241, "KEY_VIDEO_NEXT"),
    (242, "KEY_VIDEO_PREV"), (243, "KEY_BRIGHTNESS_CYCLE"), (244, "KEY_BRIGHTNESS_AUTO"), (245, "KEY_DISPLAY_OFF"),
    (246, "KEY_WIMAX"), (247, "KEY_RFKILL"), (248, "KEY_MICMUTE"), (256, "BTN_0"),
    (257, "BTN_1"), (258, "BTN_2"), (259, "BTN_3"), (260, "BTN_4"),
    (261, "BTN_5"), (262, "BTN_6"), (263, "BTN_7"), (264, "BTN_8"),
    (265, "BTN_9"), (272, "BTN_LEFT"), (273, "BTN_RIGHT"), (274, "BTN_MIDDLE"),
    (275, "BTN_SIDE"), (276, "BTN_EXTRA"), (277, "BTN_FORWARD"), (278, "BTN_BACK"),
    (279, "BTN_TASK"), (288, "BTN_JOYSTICK"), (289, "BTN_THUMB"), (290, "BTN_THUMB2"),
    (291, "BTN_TOP"), (292, "BTN_TOP2"), (293, "BTN_PINKIE"), (294, "BTN_BASE"),
    (295, "BTN_BASE2"), (296, "BTN_BASE3"), (297, "BTN_BASE4"), (298, "BTN_BASE5"),
    (299, "BTN_BASE6"), (303, "BTN_DEAD"), (304, "BTN_A"), (305, "BTN_B"),
    (306, "BTN_C"), (307, "BTN_NORTH"), (308, "BTN_WEST"), (309, "BTN_Z"),
    (310, "BTN_TL"), (311, "BTN_TR"), (312, "BTN_TL2"), (313, "BTN_TR2"),
    (314, "BTN_SELECT"), (315, "BTN_START"), (316, "BTN_MODE"), (317, "BTN_THUMBL"),
    (318, "BTN_THUMBR"), (320, "BTN_DIGI"), (321, "BTN_TOOL_RUBBER"), (322, "BTN_TOOL_BRUSH"),
    (323, "BTN_TOOL_PENCIL"), (324, "BTN_TOOL_AIRBRUSH"), (325, "BTN_TOOL_FINGER"), (326, "BTN_TOOL_MOUSE"),
    (327, "BTN_TOOL_LENS"), (328, "BTN_TOOL_QUINTTAP"), (329, "BTN_STYLUS3"), (330, "BTN_TOUCH"),
    (331, "BTN_STYLUS"), (332, "BTN_STYLUS2"), (333, "BTN_TOOL_DOUBLETAP"), (334, "BTN_TOOL_TRIPLETAP"),
    (335, "BTN_TOOL_QUADTAP"), (336, "BTN_GEAR_DOWN"), (337, "BTN_GEAR_UP"), (352, "KEY_OK"),
    (353, "KEY_SELECT"), (354, "KEY_GOTO"), (355, "KEY_CLEAR"), (356, "KEY_POWER2"),
    (357, "KEY_OPTION"), (358, "KEY_INFO"), (359, "KEY_TIME"), (360, "KEY_VENDOR"),
    (361, "KEY_ARCHIVE"), (362, "KEY_PROGRAM"), (363, "KEY_CHANNEL"), (364, "KEY_FAVORITES"),
    (365, "KEY_EPG"), (366, "KEY_PVR"), (367, "KEY_MHP"), (368, "KEY_LANGUAGE"),
    (369, "KEY_TITLE"), (370, "KEY_SUBTITLE"), (371, "KEY_ANGLE"), (372, "KEY_FULL_SCREEN"),
    (373, "KEY_MODE"), (374, "KEY_KEYBOARD"), (375, "KEY_ASPECT_RATIO"), (376, "KEY_PC"),
    (377, "KEY_TV"), (378, "KEY_TV2"), (379, "KEY_VCR"), (380, "KEY_VCR2"),
    (381, "KEY_SAT"), (382, "KEY_SAT2"), (383, "KEY_CD"), (384, "KEY_TAPE"),
    (385, "KEY_RADIO"), (386, "KEY_TUNER"), (387, "KEY_PLAYER"), (388, "KEY_TEXT"),
    (389, "KEY_DVD"), (390, "KEY_AUX"), (391, "KEY_MP3"), (392, "KEY_AUDIO"),
    (393, "KEY_VIDEO"), (394, "KEY_DIRECTORY"), (395, "KEY_LIST"), (396, "KEY_MEMO"),
    (397, "KEY_CALENDAR"), (398, "KEY_RED"), (399, "KEY_GREEN"), (400, "KEY_YELLOW"),
    (401, "KEY_BLUE"), (402, "KEY_CHANNELUP"), (403, "KEY_CHANNELDOWN"), (404, "KEY_FIRST"),
    (405, "KEY_LAST"), (406, "KEY_AB"), (407, "KEY_NEXT"), (408, "KEY_RESTART"),
    (409, "KEY_SLOW"), (410, "KEY_SHUFFLE"), (411, "KEY_BREAK"), (412, "KEY_PREVIOUS"),
    (413, "KEY_DIGITS"), (414, "KEY_TEEN"), (415, "KEY_TWEN"), (416, "KEY_VIDEOPHONE"),
    (417, "KEY_GAMES"), (418, "KEY_ZOOMIN"), (419, "KEY_ZOOMOUT"), (420, "KEY_ZOOMRESET"),
    (421, "KEY_WORDPROCESSOR"), (422, "KEY_EDITOR"), (423, "KEY_SPREADSHEET"), (424, "KEY_GRAPHICSEDITOR"),
    (425, "KEY_PRESENTATION"), (426, "KEY_DATABASE"), (427, "KEY_NEWS"), (428, "KEY_VOICEMAIL"),
    (429, "KEY_ADDRESSBOOK"), (430, "KEY_MESSENGER"), (431, "KEY_BRIGHTNESS_TOGGLE"), (432, "KEY_SPELLCHECK"),
    (433, "KEY_LOGOFF"), (434, "KEY_DOLLAR"), (435, "KEY_EURO"), (436, "KEY_FRAMEBACK"),
    (437, "KEY_FRAMEFORWARD"), (438, "KEY_CONTEXT_MENU"), (439, "KEY_MEDIA_REPEAT"), (440, "KEY_10CHANNELSUP"),
    (441, "KEY_10CHANNELSDOWN"), (442, "KEY_IMAGES"), (444, "KEY_NOTIFICATION_CENTER"), (445, "KEY_PICKUP_PHONE"),
    (446, "KEY_HANGUP_PHONE"), (447, "KEY_LINK_PHONE"), (448, "KEY_DEL_EOL"), (449, "KEY_DEL_EOS"),
    (450, "KEY_INS_LINE"), (451, "KEY_DEL_LINE"), (464, "KEY_FN"), (465, "KEY_FN_ESC"),
    (466, "KEY_FN_F1"), (467, "KEY_FN_F2"), (468, "KEY_FN_F3"), (469, "KEY_FN_F4"),
    (470, "KEY_FN_F5"), (471, "KEY_FN_F6"), (472, "KEY_FN_F7"), (473, "KEY_FN_F8"),
    (474, "KEY_FN_F9"), (475, "KEY_FN_F10"), (476, "KEY_FN_F11"), (477, "KEY_FN_F12"),
    (478, "KEY_FN_1"), (479, "KEY_FN_2"), (480, "KEY_FN_D"), (481, "KEY_FN_E"),
    (482, "KEY_FN_F"), (483, "KEY_FN_S"), (484, "KEY_FN_B"), (485, "KEY_FN_RIGHT_SHIFT"),
    (497, "KEY_BRL_DOT1"), (498, "KEY_BRL_DOT2"), (499, "KEY_BRL_DOT3"), (500, "KEY_BRL_DOT4"),
    (501, "KEY_BRL_DOT5"), (502, "KEY_BRL_DOT6"), (503, "KEY_BRL_DOT7"), (504, "KEY_BRL_DOT8"),
    (505, "KEY_BRL_DOT9"), (506, "KEY_BRL_DOT10"), (512, "KEY_NUMERIC_0"), (513, "KEY_NUMERIC_1"),
    (514, "KEY_NUMERIC_2"), (515, "KEY_NUMERIC_3"), (516, "KEY_NUMERIC_4"), (517, "KEY_NUMERIC_5"),
    (518, "KEY_NUMERIC_6"), (519, "KEY_NUMERIC_7"), (520, "KEY_NUMERIC_8"), (521, "KEY_NUMERIC_9"),
    (522, "KEY_NUMERIC_STAR"), (523, "KEY_NUMERIC_POUND"), (524, "KEY_NUMERIC_A"), (525, "KEY_NUMERIC_B"),
    (526, "KEY_NUMERIC_C"), (527, "KEY_NUMERIC_D"), (528, "KEY_CAMERA_FOCUS"), (529, "KEY_WPS_BUTTON"),
    (530, "KEY_TOUCHPAD_TOGGLE"), (531, "KEY_TOUCHPAD_ON"), (532, "KEY_TOUCHPAD_OFF"), (533, "KEY_CAMERA_ZOOMIN"),
    (534, "KEY_CAMERA_ZOOMOUT"), (535, "KEY_CAMERA_UP"), (536, "KEY_CAMERA_DOWN"), (537, "KEY_CAMERA_LEFT"),
    (538, "KEY_CAMERA_RIGHT"), (539, "KEY_ATTENDANT_ON"), (540, "KEY_ATTENDANT_OFF"), (541, "KEY_ATTENDANT_TOGGLE"),
    (542, "KEY_LIGHTS_TOGGLE"), (544, "BTN_DPAD_UP"), (545, "BTN_DPAD_DOWN"), (546, "BTN_DPAD_LEFT"),
    (547, "BTN_DPAD_RIGHT"), (548, "BTN_GRIPL"), (549, "BTN_GRIPR"), (550, "BTN_GRIPL2"),
    (551, "BTN_GRIPR2"), (560, "KEY_ALS_TOGGLE"), (561, "KEY_ROTATE_LOCK_TOGGLE"), (562, "KEY_REFRESH_RATE_TOGGLE"),
    (576, "KEY_BUTTONCONFIG"), (577, "KEY_TASKMANAGER"), (578, "KEY_JOURNAL"), (579, "KEY_CONTROLPANEL"),
    (580, "KEY_APPSELECT"), (581, "KEY_SCREENSAVER"), (582, "KEY_VOICECOMMAND"), (583, "KEY_ASSISTANT"),
    (584, "KEY_KBD_LAYOUT_NEXT"), (585, "KEY_EMOJI_PICKER"), (586, "KEY_DICTATE"), (587, "KEY_CAMERA_ACCESS_ENABLE"),
    (588, "KEY_CAMERA_ACCESS_DISABLE"), (589, "KEY_CAMERA_ACCESS_TOGGLE"), (590, "KEY_ACCESSIBILITY"), (591, "KEY_DO_NOT_DISTURB"),
    (592, "KEY_BRIGHTNESS_MIN"), (593, "KEY_BRIGHTNESS_MAX"), (594, "KEY_EPRIVACY_SCREEN_ON"), (595, "KEY_EPRIVACY_SCREEN_OFF"),
    (596, "KEY_ACTION_ON_SELECTION"), (597, "KEY_CONTEXTUAL_INSERT"), (598, "KEY_CONTEXTUAL_QUERY"), (608, "KEY_KBDINPUTASSIST_PREV"),
    (609, "KEY_KBDINPUTASSIST_NEXT"), (610, "KEY_KBDINPUTASSIST_PREVGROUP"), (611, "KEY_KBDINPUTASSIST_NEXTGROUP"), (612, "KEY_KBDINPUTASSIST_ACCEPT"),
    (613, "KEY_KBDINPUTASSIST_CANCEL"), (614, "KEY_RIGHT_UP"), (615, "KEY_RIGHT_DOWN"), (616, "KEY_LEFT_UP"),
    (617, "KEY_LEFT_DOWN"), (618, "KEY_ROOT_MENU"), (619, "KEY_MEDIA_TOP_MENU"), (620, "KEY_NUMERIC_11"),
    (621, "KEY_NUMERIC_12"), (622, "KEY_AUDIO_DESC"), (623, "KEY_3D_MODE"), (624, "KEY_NEXT_FAVORITE"),
    (625, "KEY_STOP_RECORD"), (626, "KEY_PAUSE_RECORD"), (627, "KEY_VOD"), (628, "KEY_UNMUTE"),
    (629, "KEY_FASTREVERSE"), (630, "KEY_SLOWREVERSE"), (631, "KEY_DATA"), (632, "KEY_ONSCREEN_KEYBOARD"),
    (633, "KEY_PRIVACY_SCREEN_TOGGLE"), (634, "KEY_SELECTIVE_SCREENSHOT"), (635, "KEY_NEXT_ELEMENT"), (636, "KEY_PREVIOUS_ELEMENT"),
    (637, "KEY_AUTOPILOT_ENGAGE_TOGGLE"), (638, "KEY_MARK_WAYPOINT"), (639, "KEY_SOS"), (640, "KEY_NAV_CHART"),
    (641, "KEY_FISHING_CHART"), (642, "KEY_SINGLE_RANGE_RADAR"), (643, "KEY_DUAL_RANGE_RADAR"), (644, "KEY_RADAR_OVERLAY"),
    (645, "KEY_TRADITIONAL_SONAR"), (646, "KEY_CLEARVU_SONAR"), (647, "KEY_SIDEVU_SONAR"), (648, "KEY_NAV_INFO"),
    (649, "KEY_BRIGHTNESS_MENU"), (656, "KEY_MACRO1"), (657, "KEY_MACRO2"), (658, "KEY_MACRO3"),
    (659, "KEY_MACRO4"), (660, "KEY_MACRO5"), (661, "KEY_MACRO6"), (662, "KEY_MACRO7"),
    (663, "KEY_MACRO8"), (664, "KEY_MACRO9"), (665, "KEY_MACRO10"), (666, "KEY_MACRO11"),
    (667, "KEY_MACRO12"), (668, "KEY_MACRO13"), (669, "KEY_MACRO14"), (670, "KEY_MACRO15"),
    (671, "KEY_MACRO16"), (672, "KEY_MACRO17"), (673, "KEY_MACRO18"), (674, "KEY_MACRO19"),
    (675, "KEY_MACRO20"), (676, "KEY_MACRO21"), (677, "KEY_MACRO22"), (678, "KEY_MACRO23"),
    (679, "KEY_MACRO24"), (680, "KEY_MACRO25"), (681, "KEY_MACRO26"), (682, "KEY_MACRO27"),
    (683, "KEY_MACRO28"), (684, "KEY_MACRO29"), (685, "KEY_MACRO30"), (688, "KEY_MACRO_RECORD_START"),
    (689, "KEY_MACRO_RECORD_STOP"), (690, "KEY_MACRO_PRESET_CYCLE"), (691, "KEY_MACRO_PRESET1"), (692, "KEY_MACRO_PRESET2"),
    (693, "KEY_MACRO_PRESET3"), (696, "KEY_KBD_LCD_MENU1"), (697, "KEY_KBD_LCD_MENU2"), (698, "KEY_KBD_LCD_MENU3"),
    (699, "KEY_KBD_LCD_MENU4"), (700, "KEY_KBD_LCD_MENU5"), (701, "KEY_PERFORMANCE"), (704, "BTN_TRIGGER_HAPPY"),
    (705, "BTN_TRIGGER_HAPPY2"), (706, "BTN_TRIGGER_HAPPY3"), (707, "BTN_TRIGGER_HAPPY4"), (708, "BTN_TRIGGER_HAPPY5"),
    (709, "BTN_TRIGGER_HAPPY6"), (710, "BTN_TRIGGER_HAPPY7"), (711, "BTN_TRIGGER_HAPPY8"), (712, "BTN_TRIGGER_HAPPY9"),
    (713, "BTN_TRIGGER_HAPPY10"), (714, "BTN_TRIGGER_HAPPY11"), (715, "BTN_TRIGGER_HAPPY12"), (716, "BTN_TRIGGER_HAPPY13"),
    (717, "BTN_TRIGGER_HAPPY14"), (718, "BTN_TRIGGER_HAPPY15"), (719, "BTN_TRIGGER_HAPPY16"), (720, "BTN_TRIGGER_HAPPY17"),
    (721, "BTN_TRIGGER_HAPPY18"), (722, "BTN_TRIGGER_HAPPY19"), (723, "BTN_TRIGGER_HAPPY20"), (724, "BTN_TRIGGER_HAPPY21"),
    (725, "BTN_TRIGGER_HAPPY22"), (726, "BTN_TRIGGER_HAPPY23"), (727, "BTN_TRIGGER_HAPPY24"), (728, "BTN_TRIGGER_HAPPY25"),
    (729, "BTN_TRIGGER_HAPPY26"), (730, "BTN_TRIGGER_HAPPY27"), (731, "BTN_TRIGGER_HAPPY28"), (732, "BTN_TRIGGER_HAPPY29"),
    (733, "BTN_TRIGGER_HAPPY30"), (734, "BTN_TRIGGER_HAPPY31"), (735, "BTN_TRIGGER_HAPPY32"), (736, "BTN_TRIGGER_HAPPY33"),
    (737, "BTN_TRIGGER_HAPPY34"), (738, "BTN_TRIGGER_HAPPY35"), (739, "BTN_TRIGGER_HAPPY36"), (740, "BTN_TRIGGER_HAPPY37"),
    (741, "BTN_TRIGGER_HAPPY38"), (742, "BTN_TRIGGER_HAPPY39"), (743, "BTN_TRIGGER_HAPPY40"), (767, "KEY_MAX"),
    (768, "KEY_CNT"),
];

pub(crate) const REL_TABLE: &[(u16, &str)] = &[
    (0, "REL_X"), (1, "REL_Y"), (2, "REL_Z"), (3, "REL_RX"),
    (4, "REL_RY"), (5, "REL_RZ"), (6, "REL_HWHEEL"), (7, "REL_DIAL"),
    (8, "REL_WHEEL"), (9, "REL_MISC"), (10, "REL_RESERVED"), (11, "REL_WHEEL_HI_RES"),
    (12, "REL_HWHEEL_HI_RES"), (15, "REL_MAX"), (16, "REL_CNT"),
];

// ─── table lookups ─────────────────────────────────────────────────

/// evdev code → `KEY_*`/`BTN_*` name (binary search; `None` when the code
/// has no name in the ported table — the recorder then falls back to the
/// numeric code string, exactly like Python's `by_val.get(code, str(code))`).
pub(crate) fn key_name(code: u16) -> Option<&'static str> {
    KEY_TABLE
        .binary_search_by_key(&code, |e| e.0)
        .ok()
        .map(|i| KEY_TABLE[i].1)
}

/// `KEY_*`/`BTN_*` name → evdev code (linear scan over the canonical list;
/// mirrors Python's reverse map, which only contains the alias that won the
/// `_name_table` first-declaration dedup).
pub(crate) fn key_code(name: &str) -> Option<u16> {
    KEY_TABLE.iter().find(|e| e.1 == name).map(|e| e.0)
}

/// evdev code → `REL_*` name.
pub(crate) fn rel_name(code: u16) -> Option<&'static str> {
    REL_TABLE
        .binary_search_by_key(&code, |e| e.0)
        .ok()
        .map(|i| REL_TABLE[i].1)
}

/// `REL_*` name → evdev code.
pub(crate) fn rel_code(name: &str) -> Option<u16> {
    REL_TABLE.iter().find(|e| e.1 == name).map(|e| e.0)
}

/// Every key code < 0x300 advertised on the virtual device (Python's
/// `ALL_KEYS`: uinput rejects codes >= KEY_MAX).
pub(crate) fn all_key_codes() -> impl Iterator<Item = u16> + 'static {
    KEY_TABLE.iter().map(|e| e.0).filter(|&c| c < 0x300)
}

// ─── playback scheduling math (pure, unit-tested) ─────────────────

/// Pause between repeat iterations in seconds before speed scaling.
const LOOP_GAP_S: f64 = 0.1;

/// Name of the virtual uinput device (wire-visible; same as the Python one).
const UINPUT_NAME: &str = "icue-nexus-display-macro";

/// Playback speed clamp: 0.25..=4.  `NaN` (Python's min/max chain maps it
/// to 0.25) is normalised to the documented default 1.0 instead; signed
/// infinities clamp by sign like Python's chain.
pub(crate) fn clamp_speed(speed: f64) -> f64 {
    if speed.is_nan() {
        1.0
    } else {
        speed.clamp(0.25, 4.0)
    }
}

/// Wall-clock delay for one recorded `dt` (milliseconds) at `speed`×.
pub(crate) fn event_delay(dt_ms: u64, speed: f64) -> Duration {
    Duration::from_secs_f64(dt_ms as f64 / 1000.0 / clamp_speed(speed))
}

/// Pause between repeat iterations at `speed`× (0.1 s / speed).
pub(crate) fn loop_gap(speed: f64) -> Duration {
    Duration::from_secs_f64(LOOP_GAP_S / clamp_speed(speed))
}

// ─── the player ───────────────────────────────────────────────────

/// Shared uinput state: the virtual device plus the keys currently held
/// down by the running playback (for `release_all`).
struct DevState {
    device: Option<VirtualDevice>,
    down: HashSet<u16>,
}

/// One in-flight playback run.
struct Run {
    stop_tx: Sender<()>,
    handle: JoinHandle<()>,
}

struct Book {
    run: Option<Run>,
}

pub(crate) struct Player {
    /// Thread bookkeeping: at most one run at a time.  The playback thread
    /// never locks this mutex (its release guard only locks `dev`), so
    /// joining while holding it is deadlock-free — Python's `_lock` layout.
    book: Mutex<Book>,
    dev: Arc<Mutex<DevState>>,
    int_tx: Sender<Internal>,
    ev: Sender<CoreEvent>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

impl Player {
    pub(crate) fn new(int_tx: Sender<Internal>, ev: Sender<CoreEvent>) -> Self {
        Player {
            book: Mutex::new(Book { run: None }),
            dev: Arc::new(Mutex::new(DevState {
                device: None,
                down: HashSet::new(),
            })),
            int_tx,
            ev,
        }
    }

    /// Cancel any current playback and start a new one (Python
    /// `Player.play`).  `seq` is echoed back in `Internal::Playing` so the
    /// host can ignore stale stop notifications from a cancelled run.
    /// Returns `Err` — leaving playback state untouched — when the run
    /// cannot start (empty event list, uinput unavailable, spawn failure).
    pub(crate) fn play(
        &self,
        events: Vec<MacroEvent>,
        speed: f64,
        repeat: bool,
        seq: usize,
    ) -> Result<(), String> {
        if events.is_empty() {
            return Err("no events".to_string());
        }
        let speed = clamp_speed(speed);
        let mut book = lock(&self.book);
        self.cancel_locked(&mut book);
        // Create the virtual device eagerly so a missing /dev/uinput surfaces
        // to the caller instead of silently killing the run on its first event.
        {
            let mut dev = lock(&self.dev);
            ensure_device(&mut dev)?;
        }
        let (stop_tx, stop_rx) = crossbeam_channel::unbounded();
        let handle = std::thread::Builder::new()
            .name("k100-player".to_string())
            .spawn({
                let dev = Arc::clone(&self.dev);
                let int_tx = self.int_tx.clone();
                let ev = self.ev.clone();
                move || run(events, speed, repeat, seq, stop_rx, dev, int_tx, ev)
            })
            .map_err(|e| format!("spawn player: {e}"))?;
        book.run = Some(Run { stop_tx, handle });
        Ok(())
    }

    /// Signal the current playback to stop and wait for it (Python
    /// `Player.stop`).
    pub(crate) fn stop(&self) {
        let mut book = lock(&self.book);
        self.cancel_locked(&mut book);
    }

    /// Stop playback, release held keys and drop the virtual device
    /// (Python `Player.close`).
    pub(crate) fn close(&self) {
        self.stop();
        let _book = lock(&self.book); // keep the book → dev lock order
        let mut dev = lock(&self.dev);
        dev.device = None; // dropping the uinput fd removes the node
        dev.down.clear();
    }

    fn cancel_locked(&self, book: &mut Book) {
        if let Some(run) = book.run.take() {
            let _ = run.stop_tx.send(());
            // The run thread never takes `book`, so joining under the lock
            // cannot deadlock (Python's `_cancel_locked` comment).
            let _ = run.handle.join();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.close();
    }
}

/// The playback loop (Python `Player._run`): per-event delay scaled by
/// `speed`, repeat gap, stop checks before every wait.
fn run(
    events: Vec<MacroEvent>,
    speed: f64,
    repeat: bool,
    seq: usize,
    stop_rx: Receiver<()>,
    dev: Arc<Mutex<DevState>>,
    int_tx: Sender<Internal>,
    ev: Sender<CoreEvent>,
) {
    let _ = int_tx.send(Internal::Playing { on: true, seq });
    // Drop guard: releases every held key and reports the stop on every exit
    // path — normal completion, stop(), device errors and panic unwinds.
    let _guard = ReleaseGuard {
        dev,
        int_tx,
        seq,
    };
    let dev = &_guard.dev;
    loop {
        for event in &events {
            if wait_or_stopped(&stop_rx, event_delay(event.dt, speed)) {
                return;
            }
            if let Err(e) = apply(dev, event) {
                let _ = ev.send(CoreEvent::MacroLog {
                    level: LogLevel::Error,
                    msg: format!("play: {e}"),
                });
                return;
            }
        }
        if !repeat {
            return;
        }
        if wait_or_stopped(&stop_rx, loop_gap(speed)) {
            return;
        }
    }
}

/// Wait for `d`, returning `true` when the run should stop meanwhile.
/// A zero delay just polls the stop channel (Python's `is_set()` check).
fn wait_or_stopped(rx: &Receiver<()>, d: Duration) -> bool {
    if d.is_zero() {
        return !matches!(rx.try_recv(), Err(TryRecvError::Empty));
    }
    match rx.recv_timeout(d) {
        Ok(()) | Err(RecvTimeoutError::Disconnected) => true,
        Err(RecvTimeoutError::Timeout) => false,
    }
}

/// Applies one macro event to the uinput device.  Unknown code names are
/// skipped silently (Python `_apply` returns when the name misses); the
/// key value is normalised to 1/0 like `1 if value else 0`.
fn apply(dev: &Mutex<DevState>, event: &MacroEvent) -> Result<(), String> {
    match event.kind {
        MacroEventKind::Key => {
            let Some(code) = key_code(&event.code) else {
                return Ok(());
            };
            let value = i32::from(event.value != 0);
            let mut d = lock(dev);
            let ui = ensure_device(&mut d)?;
            ui.emit(&[InputEvent::new(EventType::KEY.0, code, value)])
                .map_err(|e| e.to_string())?;
            if value != 0 {
                d.down.insert(code);
            } else {
                d.down.remove(&code);
            }
            Ok(())
        }
        MacroEventKind::Rel => {
            let Some(code) = rel_code(&event.code) else {
                return Ok(());
            };
            let mut d = lock(dev);
            let ui = ensure_device(&mut d)?;
            ui.emit(&[InputEvent::new(EventType::RELATIVE.0, code, event.value)])
                .map_err(|e| e.to_string())?;
            Ok(())
        }
    }
}

fn ensure_device(dev: &mut DevState) -> Result<&mut VirtualDevice, String> {
    if dev.device.is_none() {
        dev.device = Some(build_device().map_err(|e| format!("uinput open failed: {e}"))?);
    }
    dev.device
        .as_mut()
        .ok_or_else(|| "uinput device unavailable".to_string())
}

/// Virtual device: every key code < 0x300 plus the REL axes k100play
/// declared (X, Y, WHEEL, HWHEEL, DIAL, WHEEL_HI_RES, HWHEEL_HI_RES).
/// `VirtualDevice::emit` appends the SYN_REPORT, matching Python's
/// `ui.write(...); ui.syn()` pairs.
fn build_device() -> std::io::Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for code in all_key_codes() {
        keys.insert(KeyCode::new(code));
    }
    let mut rels = AttributeSet::<RelativeAxisCode>::new();
    for &code in &[
        RelativeAxisCode::REL_X,
        RelativeAxisCode::REL_Y,
        RelativeAxisCode::REL_WHEEL,
        RelativeAxisCode::REL_HWHEEL,
        RelativeAxisCode::REL_DIAL,
        RelativeAxisCode::REL_WHEEL_HI_RES,
        RelativeAxisCode::REL_HWHEEL_HI_RES,
    ] {
        rels.insert(code);
    }
    VirtualDevice::builder()?
        .name(UINPUT_NAME)
        .with_keys(&keys)?
        .with_relative_axes(&rels)?
        .build()
}

/// Releases every key the run still holds and reports `Playing { on: false }.
/// Runs on normal completion, on stop, and while unwinding a panic, so a
/// modifier can never stay stuck down.
struct ReleaseGuard {
    dev: Arc<Mutex<DevState>>,
    int_tx: Sender<Internal>,
    seq: usize,
}

impl Drop for ReleaseGuard {
    fn drop(&mut self) {
        {
            let mut dev = lock(&self.dev);
            let ups: Vec<InputEvent> = dev
                .down
                .iter()
                .map(|&code| InputEvent::new(EventType::KEY.0, code, 0))
                .collect();
            if !ups.is_empty() {
                if let Some(ui) = dev.device.as_mut() {
                    let _ = ui.emit(&ups); // one batch → one SYN_REPORT
                }
            }
            dev.down.clear();
        }
        let _ = self
            .int_tx
            .send(Internal::Playing { on: false, seq: self.seq });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_clamp_bounds() {
        assert_eq!(clamp_speed(1.0), 1.0);
        assert_eq!(clamp_speed(2.5), 2.5);
        assert_eq!(clamp_speed(0.0), 0.25);
        assert_eq!(clamp_speed(-5.0), 0.25);
        assert_eq!(clamp_speed(100.0), 4.0);
        assert_eq!(clamp_speed(f64::INFINITY), 4.0);
        assert_eq!(clamp_speed(f64::NEG_INFINITY), 0.25);
        // Documented deviation from Python's min/max chain (which yields
        // 0.25 for NaN): NaN normalises to the default speed 1.0.
        assert_eq!(clamp_speed(f64::NAN), 1.0);
    }

    #[test]
    fn event_scheduling_math() {
        assert_eq!(event_delay(1000, 2.0), Duration::from_millis(500));
        assert_eq!(event_delay(250, 4.0), Duration::from_millis(62) + Duration::from_micros(500));
        assert_eq!(event_delay(0, 4.0), Duration::ZERO);
        assert_eq!(event_delay(100, 0.25), Duration::from_millis(400));
        // speed is re-clamped internally, so out-of-range input can't panic
        assert_eq!(event_delay(1000, f64::NAN), Duration::from_millis(1000));

        assert_eq!(loop_gap(1.0), Duration::from_millis(100));
        assert_eq!(loop_gap(4.0), Duration::from_millis(25));
        assert_eq!(loop_gap(0.25), Duration::from_millis(400));
    }

    #[test]
    fn code_tables_sorted_and_bijective() {
        assert!(KEY_TABLE.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(REL_TABLE.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(KEY_TABLE.len(), 629); // python-evdev _name_table size
        assert_eq!(REL_TABLE.len(), 15);
    }

    #[test]
    fn key_and_rel_names_never_collide() {
        // KEY_7 == 8 == REL_WHEEL: per-type maps must disambiguate.
        assert_eq!(key_name(8), Some("KEY_7"));
        assert_eq!(rel_name(8), Some("REL_WHEEL"));
        assert_eq!(key_code("KEY_7"), Some(8));
        assert_eq!(rel_code("REL_WHEEL"), Some(8));
        assert_eq!(key_name(30), Some("KEY_A"));
        assert_eq!(key_code("KEY_A"), Some(30));
        assert_eq!(key_code("BTN_LEFT"), Some(272));
        assert_eq!(rel_code("REL_HWHEEL_HI_RES"), Some(12));
        // alias that lost python's first-wins dedup is not resolvable
        assert_eq!(key_code("KEY_ROTATE_DISPLAY"), None);
        assert_eq!(key_code("NOPE"), None);
        assert_eq!(key_name(9999), None);
        assert_eq!(rel_name(9999), None);
    }

    #[test]
    fn uinput_key_set_matches_python_all_keys() {
        let codes: Vec<u16> = all_key_codes().collect();
        assert_eq!(codes.len(), 628); // python ALL_KEYS length
        assert!(codes.iter().all(|&c| c < 0x300));
        assert_eq!(codes.iter().min(), Some(&0));
        assert_eq!(codes.iter().max(), Some(&767)); // KEY_MAX
        // KEY_CNT (768) is >= 0x300 and must be excluded
        assert!(!codes.contains(&768));
    }

    #[test]
    fn wait_or_stopped_zero_delay_polls() {
        let (tx, rx) = crossbeam_channel::unbounded();
        assert!(!wait_or_stopped(&rx, Duration::ZERO)); // no stop pending
        let _ = tx.send(());
        assert!(wait_or_stopped(&rx, Duration::ZERO)); // stop delivered
        drop(tx);
        assert!(wait_or_stopped(&rx, Duration::ZERO)); // sender gone → stop
    }

    #[test]
    fn wait_or_stopped_wakes_on_send() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let start = std::time::Instant::now();
        let _ = tx.send(());
        // a long wait must return immediately once the stop signal arrives
        assert!(wait_or_stopped(&rx, Duration::from_secs(30)));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    /// Builds the virtual device but emits nothing — verifies /dev/uinput
    /// access and the capability set on a real machine.
    #[test]
    #[ignore = "needs /dev/uinput write access"]
    fn hardware_uinput_device_builds() {
        let dev = build_device().expect("uinput device");
        // construction only: no keys are ever injected by this test
        drop(dev);
    }
}
