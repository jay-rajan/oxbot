// The page: the sidebar and the open bot's chat, the dialogs, and the toast.
// Same structure, ids and classes as the hand-written page, so its styles
// (legacy.css) and the end-to-end tests apply as they are.
import { Toast } from "./components/Toast";
import { BotSettingsDialog } from "./screens/BotSettings/BotSettingsDialog";
import { ChatMain } from "./screens/Chat/ChatMain";
import { ReportDialog } from "./screens/Chat/ReportDialog";
import { CaseDialog } from "./screens/Compare/CaseDialog";
import { CompareDialog } from "./screens/Compare/CompareDialog";
import { SettingsDialog } from "./screens/Settings/SettingsDialog";
import { NewBotDialog } from "./screens/Sidebar/NewBotDialog";
import { Sidebar } from "./screens/Sidebar/Sidebar";
import { NameDialog } from "./screens/Welcome/NameDialog";
import { WelcomeDialog } from "./screens/Welcome/WelcomeDialog";

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
      <Toast />
    </>
  );
}
