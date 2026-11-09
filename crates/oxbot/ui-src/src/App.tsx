// The page: the sidebar and the open bot's chat, the dialogs, and the toast.
// Same structure, ids and classes as the hand-written page, so the
// end-to-end tests apply as they are. The frame's layout is in shell.css.
import { Toast } from "./components/ui";
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
  return (
    <>
      <div className="app">
        <Sidebar />
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
