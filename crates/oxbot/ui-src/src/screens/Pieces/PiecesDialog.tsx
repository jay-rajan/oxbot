// Every piece of the design system on one page, for checking the theme in
// light and dark and at phone width. The owner's developer tools only
// (Settings → General, or the page address ending in #pieces).
import { useCallback, useEffect, useState } from "react";
import { useApp, useSel } from "../../app/context";
import {
  Banner,
  Button,
  Card,
  Checkbox,
  Chip,
  Dialog,
  DialogFrame,
  Field,
  IconButton,
  Input,
  List,
  ListRow,
  MoreIcon,
  Note,
  Pill,
  PlusIcon,
  Section,
  Segmented,
  Select,
  Switch,
  Tabs,
  TextArea,
  vars,
} from "../../components/ui";
import { local } from "../../state/store";
import { isOwner } from "../Compare/devTools";
import "./pieces.css";

const COLORS = ["bg", "side", "surface", "sunken", "ink", "muted", "line", "accent", "ok", "danger", "warn"] as const;

export function PiecesDialog() {
  const { dispatch, toast } = useApp();
  const owner = useSel((s) => isOwner(s.connection.settings));
  const open = useSel((s) => s.ui.dialog === "pieces") && owner;
  const close = useCallback(() => dispatch(local("ui/close", { dialog: "pieces" })), [dispatch]);
  // The address ending in #pieces opens it (once the owner is known).
  useEffect(() => {
    if (owner && window.location.hash === "#pieces") dispatch(local("ui/open", { dialog: "pieces" }));
  }, [owner, dispatch]);
  const [seg, setSeg] = useState<"everyone" | "me" | "people">("everyone");
  const [tab, setTab] = useState<"a" | "b" | "c">("a");
  const [on, setOn] = useState(true);
  return (
    <Dialog id="piecesDlg" open={open} onClose={close} size="xl" labelledBy="piecesTitle">
      <DialogFrame title="Design pieces" titleId="piecesTitle" onClose={close} bodyClassName="pieces">
        <Section title="Colors">
          <div className="pieces-swatches">
            {COLORS.map((c) => (
              <span key={c} className="pieces-swatch">
                <i style={vars({ swatch: `var(--${c})` })} />
                <small>{c}</small>
              </span>
            ))}
          </div>
        </Section>
        <Section title="Type">
          <div className="pieces-type">
            <span className="t-xl">Welcome to oxbot</span>
            <span className="t-lg">Bot settings</span>
            <span className="t-md">Find dinner recipes when asked.</span>
            <span className="t-sm">Changing it starts a fresh conversation.</span>
            <span className="t-xs">9:41 AM</span>
          </div>
        </Section>
        <Section title="Buttons">
          <div className="btn-row">
            <Button kind="primary">Save</Button>
            <Button>Cancel</Button>
            <Button kind="quiet">Rename</Button>
            <Button kind="danger">Delete bot</Button>
            <Button kind="primary" disabled>
              Save
            </Button>
          </div>
          <div className="btn-row">
            <Button size="sm">Pause</Button>
            <Button size="sm" kind="quiet">
              Edit
            </Button>
            <Button kind="quiet" icon={<PlusIcon />}>
              New section
            </Button>
            <IconButton label="New bot">
              <PlusIcon />
            </IconButton>
            <IconButton label="More">
              <MoreIcon />
            </IconButton>
            <IconButton label="Dictate" pressed>
              <MoreIcon />
            </IconButton>
          </div>
        </Section>
        <Section title="Fields">
          <div className="pieces-two">
            <Field label="Name" hint="Shown in the list.">
              <Input defaultValue="Recipes" />
            </Field>
            <Field label="Kind of job" error="Pick one.">
              <Select defaultValue="">
                <option value="">Automatic</option>
                <option value="research">Research</option>
              </Select>
            </Field>
          </div>
          <Field label="What it's for">
            <TextArea defaultValue="Find dinner recipes when asked." rows={3} />
          </Field>
        </Section>
        <Section title="Choices">
          <Segmented
            name="piecesSeg"
            label="Shared with"
            value={seg}
            onChange={setSeg}
            options={[
              ["everyone", "Everyone"],
              ["me", "Just me"],
              ["people", "Picked people"],
            ]}
          />
          <Checkbox defaultChecked>Private: only you can see and use it</Checkbox>
          <Switch checked={on} onChange={(e) => setOn(e.target.checked)}>
            Start at login
          </Switch>
          <Tabs
            label="Example"
            value={tab}
            onChange={setTab}
            tabs={[
              ["a", "General"],
              ["b", "Websites"],
              ["c", "People"],
            ]}
          />
        </Section>
        <Section title="Lists" actions={<Button size="sm" kind="quiet">Add</Button>}>
          <List>
            <ListRow
              lead={<span className="person-dot" style={vars({ person: "var(--accent)" })} />}
              title="Jay"
              sub="Owner · this Mac"
              trail={<Button size="sm">Edit</Button>}
            />
            <ListRow lead={<span className="dot on" />} title="Divit's iPhone" sub="Connected" trail={<Pill tone="ok">on</Pill>} />
            <ListRow title="Fridays 9:00 AM" sub="Plan the week" trail={<Button size="sm">Pause</Button>} onClick={() => toast("Row pressed")} />
          </List>
          <Note>Nothing yet.</Note>
        </Section>
        <Section title="Chips">
          <div className="btn-row">
            <Chip sub="2 MB">menu.pdf</Chip>
            <Chip onRemove={() => toast("Removed")}>photo.png</Chip>
            <Chip tone="bad">too-big.zip is over 25 MB</Chip>
            <Chip onClick={() => toast("Opened")}>notes.csv</Chip>
            <Pill>3</Pill>
            <Pill tone="accent">new</Pill>
          </div>
        </Section>
        <Section title="Banners and cards">
          <Banner lead={<span className="dot off" />} actions={<Button size="sm">Retry</Button>}>
            Reconnecting to the home computer…
          </Banner>
          <Banner tone="warn">You've used 80% of this month's AI.</Banner>
          <Banner tone="danger">Couldn't reach OpenRouter.</Banner>
          <Card tone="warn" title="Recipes wants to open allrecipes.com">
            <span className="muted">To look up tonight's dinner.</span>
            <div className="btn-row">
              <Button kind="primary">Allow</Button>
              <Button>Deny</Button>
            </div>
          </Card>
          <Card title="Sign in to allrecipes.com">
            <div className="btn-row">
              <Button kind="primary">Open sign-in</Button>
            </div>
          </Card>
        </Section>
        <Section title="Toast">
          <div className="btn-row">
            <Button onClick={() => toast("Saved.")}>Show a toast</Button>
            <Button kind="danger" onClick={() => toast("Couldn't save.", true)}>
              Show an error
            </Button>
          </div>
        </Section>
      </DialogFrame>
    </Dialog>
  );
}
