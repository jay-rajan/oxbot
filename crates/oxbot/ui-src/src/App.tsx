// The page: the sidebar and the open bot's chat, the dialogs, and the toast.
// Same structure, ids and classes as the hand-written page, so the
// end-to-end tests apply as they are. The frame's layout is in shell.css.
import { useEffect } from "react";
import { useSel } from "./app/context";
import { Toast } from "./components/ui";
import { CalendarView } from "./screens/Calendar/CalendarView";
import { NotesView } from "./screens/Notes/NotesView";
import { MealsView } from "./screens/Meals/MealsView";
import { GroceriesView } from "./screens/Meals/GroceriesView";
import { useHomeMode, useSavedHomeMode } from "./screens/Calendar/HomeMode";
import { BotSettingsDialog } from "./screens/BotSettings/BotSettingsDialog";
import { ChatMain } from "./screens/Chat/ChatMain";
import { ReportDialog } from "./screens/Chat/ReportDialog";
import { CaseDialog } from "./screens/Compare/CaseDialog";
import { CompareDialog } from "./screens/Compare/CompareDialog";
import { PiecesDialog } from "./screens/Pieces/PiecesDialog";
import { SettingsDialog } from "./screens/Settings/SettingsDialog";
import { NewBotDialog } from "./screens/Sidebar/NewBotDialog";
import { PhoneTabs } from "./screens/Sidebar/PhoneTabs";
import { Sidebar } from "./screens/Sidebar/Sidebar";
import { NameDialog } from "./screens/Welcome/NameDialog";
import { WelcomeDialog } from "./screens/Welcome/WelcomeDialog";
import "./shell.css";

export function App() {
  const page = useSel((s) => s.ui.page);
  const mode = useHomeMode();
  useSavedHomeMode();
  // In a chat the phone's tab bar steps away (shell.css), until a tap on
  // the chat or the end of typing brings it up.
  const tabsUp = useSel((s) => s.ui.tabsUp);
  const away = (page === "chat" || mode === "chat") && !tabsUp;
  useEffect(() => {
    document.documentElement.classList.toggle("tabs-away", away);
  }, [away]);
  return (
    <>
      {/* A Home view sits beside the chat on a wide screen (calendar.css). */}
      <div className={page === "chat" ? "app" : `app home mode-${mode}`}>
        <Sidebar />
        {page === "calendar" ? <CalendarView /> : null}
        {page === "notes" ? <NotesView /> : null}
        {page === "meals" ? <MealsView /> : null}
        {page === "groceries" ? <GroceriesView /> : null}
        <ChatMain />
      </div>
      <PhoneTabs />
      <NewBotDialog />
      <BotSettingsDialog />
      <ReportDialog />
      <SettingsDialog />
      <WelcomeDialog />
      <NameDialog />
      <CaseDialog />
      <CompareDialog />
      <PiecesDialog />
      <Toast />
    </>
  );
}
