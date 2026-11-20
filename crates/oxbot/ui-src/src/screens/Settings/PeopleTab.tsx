// Settings → People: the home computer that runs the bots, then the
// household (up to six people). For the home's administrators (on the home
// computer or any device) each person has a QR code to join their phone and
// an edit button (rename, Administrator, their phones, Remove from home);
// Add person adds someone by name. Your own row edits
// your details. The Wi-Fi networks phones may use are hidden: pairing lets
// phones use the network the home computer is on.
import { type FormEvent, useEffect, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Banner, Button, cx, ErrorText, IconButton, Input, List, ListRow, PlusIcon, Section, Switch, vars } from "../../components/ui";
import type { DeviceInfo, Person } from "../../net/protocol";
import { local } from "../../state/store";
import { ProfileFields, useProfileDraft } from "../Welcome/ProfileFields";
import { profileEdit } from "../Welcome/profile";
import { doorHere, seenText } from "./format";
import { useArm } from "./useArm";

export function PeopleTab({ hidden }: { hidden: boolean }) {
  return (
    <div id="setPeople" className="settings-pane" role="tabpanel" hidden={hidden}>
      <HomeComputerRow />
      <Section title={<HouseholdTitle />}>
        <PeopleRows />
        <AddPerson />
      </Section>
      <p className="note-text">Add a person, then tap the QR code next to their name and scan it with the oxbot app on their phone. They join the home's bots over your Wi-Fi.</p>
    </div>
  );
}

const MAX_PEOPLE = 6;

function HouseholdTitle() {
  const people = useSel((s) => s.home.people.length);
  return <>{`People · ${people} of ${MAX_PEOPLE}`}</>;
}

/** What kind of machine runs the bots: today always the family's own Mac
 *  (a rented cloud computer comes later, docs/plan/cloud-setups.html). */
export function machineKind(name: string | null | undefined): string {
  return /macbook|laptop/i.test(name || "") ? "Personal laptop" : "Personal computer";
}

/** The machine that runs the bots, named for what it is. */
function HomeComputerRow() {
  const mac = useSel((s) => s.connection.serverName);
  const here = useSel((s) => !!s.connection.settings?.home_computer);
  return (
    <Section title="Runs the bots">
      <List className="people-rows">
        <ListRow
          lead={
            <span className="machine-icon" aria-hidden="true">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                <rect x="4" y="5" width="16" height="11" rx="1.5" />
                <path d="M2.5 19h19" />
              </svg>
            </span>
          }
          title={mac || "The home computer"}
          sub={`${machineKind(mac)}${here ? " · you're on it now" : ""}`}
        />
      </List>
    </Section>
  );
}

/** The pairing code, while it still works (checked every 15 s). */
function useLivePairing() {
  const pairing = useSel((s) => s.home.pairing);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!pairing) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), 15000);
    return () => clearInterval(t);
  }, [pairing]);
  return pairing && pairing.expires_ms >= now ? { pairing, now } : null;
}

/** Shows a pairing code for `person`'s phone (the door opens for it). */
function useStartPairing() {
  const { send, dispatch } = useApp();
  const door = useSel((s) => s.home.door);
  return (person: string) => {
    const { here, listed } = doorHere(door);
    if (!here) {
      dispatch(local("activity/pairWhy", { why: door?.why || "The home computer isn't on a Wi-Fi network phones can join." }));
      return;
    }
    dispatch(local("activity/pairWhy", { why: null }));
    // Phones may use the network the home computer is on now.
    if (!listed) send({ type: "add_door_network" });
    send({ type: "start_pairing", person });
  };
}

const QrIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
    <rect x="3.5" y="3.5" width="6.5" height="6.5" rx="1" />
    <rect x="14" y="3.5" width="6.5" height="6.5" rx="1" />
    <rect x="3.5" y="14" width="6.5" height="6.5" rx="1" />
    <path d="M14 14h2.5v2.5H14zM18 18h2.5v2.5H18zM18 14h2.5M14 18v2.5" strokeLinecap="round" />
  </svg>
);

const EditIcon = () => (
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M4 20h4L19 9l-4-4L4 16v4zM13.5 6.5l4 4" />
  </svg>
);

/** The QR code for one person's phone, under their row. */
function PairingBox() {
  const { send, toast } = useApp();
  const live = useLivePairing();
  const pairing = live?.pairing ?? null;
  if (!pairing || !live) return null;
  const mins = Math.max(0, Math.ceil((pairing.expires_ms - live.now) / 60000));
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(pairing.link);
      toast("Link copied. Send it to the phone some private way, e.g. AirDrop.");
    } catch {
      toast("Couldn't copy the link.", true);
    }
  };
  return (
    <div className="pairing" id="pairBox">
      <div
        className="qr"
        id="pairQr"
        aria-label="QR code to scan with the oxbot app on the phone"
        // oxbot's own drawing of the code, not anyone's text.
        dangerouslySetInnerHTML={{ __html: pairing.qr_svg.replace(/^<\?xml[^>]*>/, "") }}
      />
      <div className="pair-text">
        <p>
          In the oxbot app on {pairing.person}'s phone, tap <b>Scan the code</b> and point it here.
        </p>
        <p className="code" id="pairCode">
          {pairing.code}
        </p>
        <p className="field-hint" id="pairWhen">{`Works once, for ${mins} more minute${mins === 1 ? "" : "s"}.`}</p>
        <div className="btn-row">
          <Button size="sm" id="pairCopy" onClick={copy}>
            Copy link
          </Button>
          <Button kind="quiet" size="sm" id="pairCancel" onClick={() => send({ type: "cancel_pairing" })}>
            Done
          </Button>
        </div>
      </div>
    </div>
  );
}

/** + Add person: a name, and they pick the rest on their phone. */
function AddPerson() {
  const { send, dispatch } = useApp();
  const admin = useSel((s) => !!s.connection.settings?.admin);
  const people = useSel((s) => s.home.people);
  const open = useSel((s) => s.activity.pairFormOpen);
  const [name, setName] = useState("");
  const [err, setErr] = useState<string | null>(null);
  if (!admin) return null;
  const full = people.length >= MAX_PEOPLE;
  if (!open)
    return (
      <div className="btn-row">
        <Button id="addPerson" icon={<PlusIcon />} disabled={full} title={full ? `A home has up to ${MAX_PEOPLE} people` : undefined} onClick={() => dispatch(local("activity/pairForm", { open: true }))}>
          Add person
        </Button>
      </div>
    );
  const submit = (ev: FormEvent) => {
    ev.preventDefault();
    const n = name.trim();
    if (!n) return setErr("Say what to call them.");
    if (people.some((p) => p.name.toLowerCase() === n.toLowerCase())) return setErr(`${n} is already in the home.`);
    send({ type: "add_person", name: n });
    setName("");
    setErr(null);
    dispatch(local("activity/pairForm", { open: false }));
  };
  return (
    <form className="add-site" id="addPersonForm" onSubmit={submit}>
      <Input id="addPersonName" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="Their name, e.g. Sam" aria-label="Their name" autoComplete="off" spellCheck={false} maxLength={40} />
      <Button kind="primary" type="submit">
        Add
      </Button>
      <Button kind="quiet" onClick={() => dispatch(local("activity/pairForm", { open: false }))}>
        Cancel
      </Button>
      <ErrorText hidden={!err}>{err}</ErrorText>
    </form>
  );
}

function PeopleRows() {
  const { send, dispatch } = useApp();
  const settings = useSel((s) => s.connection.settings);
  const people = useSel((s) => s.home.people);
  const devices = useSel((s) => s.home.devices);
  const personOpen = useSel((s) => s.activity.personOpen);
  const why = useSel((s) => s.activity.pairWhy);
  const live = useLivePairing();
  const startPairing = useStartPairing();
  const arm = useArm();
  // The home computer and administrators manage people, on any device.
  const admin = !!settings?.admin;
  const you = settings?.you;
  const admins = people.filter((p) => p.admin).length;

  // Everyone, and anyone with a device who isn't listed yet.
  const list: Person[] = [...people];
  for (const g of devices)
    if (!list.some((p) => p.name === g.person))
      list.push({ name: g.person, color: people.find((p) => p.name === g.person)?.color || "var(--muted)", role: "adult", photo: null, admin: false });

  return (
    <List className="people-rows" id="peopleRows">
      {list.map((p) => {
        const devs = devices.filter((g) => g.person === p.name);
        const owner = p.role === "owner";
        const role = p.admin ? "admin" : p.role === "kid" ? "kid account" : "";
        const devText = devs.map((g) => `${g.name}, ${seenText(g)}`).join(" · ") || "No phone yet";
        const mine = p.name === you;
        const canEdit = mine || admin;
        const isOpen = personOpen === p.name;
        const pairingHere = live?.pairing.person === p.name;
        return (
          <ListRow
            key={p.name}
            className="person-row"
            data-person={p.name}
            lead={
              p.photo?.startsWith("data:image/") ? (
                <img className="person-photo" src={p.photo} alt="" />
              ) : (
                <span className="person-dot maker-dot" style={vars({ person: p.color })} />
              )
            }
            title={p.name + (role ? ` (${role})` : "")}
            sub={devText}
            trail={
              <span className="person-acts">
                {admin ? (
                  <IconButton label={`Show a QR code for ${p.name} to scan from their phone`} className={cx("qr-btn", pairingHere && "on")} onClick={() => (pairingHere ? send({ type: "cancel_pairing" }) : startPairing(p.name))}>
                    <QrIcon />
                  </IconButton>
                ) : null}
                {canEdit ? (
                  <IconButton
                    label={isOpen ? `Done editing ${p.name}` : `Edit ${p.name}`}
                    aria-expanded={isOpen}
                    className={cx(isOpen && "on")}
                    onClick={() => {
                      if (!isOpen && mine) send({ type: "get_profile" });
                      dispatch(local("activity/person", { name: p.name }));
                    }}
                  >
                    <EditIcon />
                  </IconButton>
                ) : null}
              </span>
            }
          >
            {pairingHere ? <div className="person-edit"><PairingBox /></div> : null}
            {isOpen ? (
              <div className="person-edit">
                {mine ? <MeForm /> : admin && !owner ? <RenameForm name={p.name} /> : null}
                {admin && p.role !== "kid" ? <AdminSwitch person={p} last={p.admin && admins <= 1} /> : null}
                {admin && !owner && !p.admin ? <KidSwitch person={p} /> : null}
                {admin && devs.length ? (
                  <List className="device-list">
                    {devs.map((g) => (
                      <DeviceRow key={g.id} g={g} armed={arm.armed === g.id} onRemove={() => arm.fire(g.id) && send({ type: "remove_device", id: g.id })} />
                    ))}
                  </List>
                ) : null}
                {admin && !owner && !(p.admin && admins <= 1) ? (
                  <div className="btn-row">
                    <Button kind="quiet" className="danger-text" id="removePerson" onClick={() => arm.fire(`p:${p.name}`) && send({ type: "remove_person", name: p.name })}>
                      {arm.armed === `p:${p.name}` ? `Yes, remove ${p.name}` : "Remove from home"}
                    </Button>
                    <small className="field-hint">Their phones stop working; their bots and chats stay.</small>
                  </div>
                ) : null}
              </div>
            ) : null}
          </ListRow>
        );
      })}
      {why ? (
        <li className="list-row no-lead">
          <Banner tone="warn" id="pairWhy">
            {why}
          </Banner>
        </li>
      ) : null}
    </List>
  );
}

/** Administrator or not: sees and manages every bot (private ones too),
 *  the people and the home's settings, from any device. A home keeps one. */
function AdminSwitch({ person, last }: { person: Person; last: boolean }) {
  const { send } = useApp();
  return (
    <div className="admin-switch">
      <Switch id={`admin-${person.name}`} checked={person.admin} disabled={last} onChange={(e) => send({ type: "set_admin", name: person.name, on: e.target.checked })}>
        Administrator
      </Switch>
      <small className="field-hint">{last ? "A home needs at least one administrator. Make someone else one first." : "Sees every bot, private ones too, and manages the people and the home's settings from any device."}</small>
    </div>
  );
}

/** Under 18 (a kid account) or not. Administrators can't be kids. */
function KidSwitch({ person }: { person: Person }) {
  const { send } = useApp();
  return (
    <div className="admin-switch">
      <Switch id={`kid-${person.name}`} checked={person.role === "kid"} onChange={(e) => send({ type: "set_kid", name: person.name, on: e.target.checked })}>
        Under 18
      </Switch>
      <small className="field-hint">
        Their messages are checked first, and anything a parent should see waits for an administrator. Photos and files always wait for an administrator's OK.
      </small>
    </div>
  );
}

/** Someone else's name, changed by an administrator. */
function RenameForm({ name }: { name: string }) {
  const { send } = useApp();
  const [to, setTo] = useState(name);
  return (
    <form
      className="add-site"
      onSubmit={(ev) => {
        ev.preventDefault();
        if (to.trim() && to.trim() !== name) send({ type: "rename_person", name, to: to.trim() });
      }}
    >
      <Input value={to} onChange={(e) => setTo(e.target.value)} aria-label={`${name}'s name`} autoComplete="off" spellCheck={false} maxLength={40} />
      <Button type="submit" disabled={!to.trim() || to.trim() === name}>
        Rename
      </Button>
    </form>
  );
}

function DeviceRow({ g, armed, onRemove }: { g: DeviceInfo; armed: boolean; onRemove: () => void }) {
  return (
    <ListRow
      className="device-row"
      lead={<i className={cx("dot", g.online && "on")} />}
      title={g.name}
      sub={seenText(g)}
      trail={
        <Button kind="quiet" size="sm" onClick={onRemove}>
          {armed ? "Yes, remove" : "Remove"}
        </Button>
      }
    />
  );
}

/** Your details: the same fields as the welcome screen. */
function MeForm() {
  const { send, toast } = useApp();
  const profile = useSel((s) => s.home.profile);
  const view = profile && !profile.welcome ? profile : null;
  const { formRef, draft, setDraft } = useProfileDraft(view);
  const [err, setErr] = useState<string | null>(null);
  const save = (ev: FormEvent) => {
    ev.preventDefault();
    if (view && draft) {
      const r = profileEdit(draft, view);
      if ("error" in r) {
        setErr(r.error);
        return;
      }
      setErr(null);
      send({ type: "set_profile", profile: r.edit });
    }
    toast("Saved.");
  };
  return (
    <form id="meForm" className="profile-form" ref={formRef} onSubmit={save}>
      {view && draft ? <ProfileFields boxId="meFields" view={view} draft={draft} onChange={setDraft} /> : <div id="meFields" />}
      <ErrorText id="meErr" hidden={!err}>
        {err}
      </ErrorText>
      <div className="btn-row">
        <Button kind="primary" type="submit">
          Save
        </Button>
      </div>
    </form>
  );
}
