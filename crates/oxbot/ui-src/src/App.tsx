// The page: the sidebar and the open bot's chat, the dialogs, and the toast.
// Same structure, ids and classes as the hand-written page, so the
// end-to-end tests apply as they are. The frame's layout is in shell.css.
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
import { Sidebar } from "./screens/Sidebar/Sidebar";
import { NameDialog } from "./screens/Welcome/NameDialog";
import { WelcomeDialog } from "./screens/Welcome/WelcomeDialog";
import "./shell.css";

export function App() {
  const page = useSel((s) => s.ui.page);
  const mode = useHomeMode();
  useSavedHomeMode();
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
