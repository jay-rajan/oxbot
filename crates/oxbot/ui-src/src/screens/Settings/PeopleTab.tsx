// Settings → People: each person with their devices. Your own row opens your
// details; on the home computer, anyone's row lists their devices with
// Remove, and Add a device shows a pairing code (QR) for a phone, tablet or
// laptop. The Wi-Fi networks phones may use are hidden: pairing lets phones
// use the network the home computer is on.
import { type FormEvent, useEffect, useRef, useState } from "react";
import { useApp, useSel } from "../../app/context";
import { Banner, Button, cx, ErrorText, Input, List, ListRow, PlusIcon, Section, vars } from "../../components/ui";
import type { GateInfo, Person } from "../../net/protocol";
import { local } from "../../state/store";
import { ProfileFields, useProfileDraft } from "../Welcome/ProfileFields";
import { profileEdit } from "../Welcome/profile";
import { doorHere, seenText } from "./format";
import { useArm } from "./useArm";

export function PeopleTab({ hidden }: { hidden: boolean }) {
  return (
    <div id="setPeople" className="settings-pane" role="tabpanel" hidden={hidden}>
      <Section>
        <PeopleRows />
      </Section>
      <Section>
        <Pairing />
        <p className="note-text">A device is a phone, tablet or laptop that uses the home's bots over your Wi-Fi.</p>
      </Section>
    </div>
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

function Pairing() {
  const { send, dispatch, toast } = useApp();
  const settings = useSel((s) => s.connection.settings);
  const people = useSel((s) => s.home.people);
  const door = useSel((s) => s.home.door);
  const formOpen = useSel((s) => s.activity.pairFormOpen);
  const why = useSel((s) => s.activity.pairWhy);
  const live = useLivePairing();
  const pairing = live?.pairing ?? null;
  const farm = !!settings?.farm;
  const you = settings?.you ?? "";
  const personRef = useRef<HTMLInputElement>(null);

  // Add a device: your own name to start with, ready to change.
  useEffect(() => {
    const input = personRef.current;
    if (!formOpen || !input) return;
    input.value = input.value || you;
    input.focus();
    input.select();
  }, [formOpen, you]);

  const submit = (ev: FormEvent) => {
    ev.preventDefault();
    const person = personRef.current?.value.trim() || you;
    if (!person) {
      toast("Say who the device is for.", true);
      return;
    }
    const { here, listed } = doorHere(door);
    if (!here) {
      dispatch(local("activity/pairWhy", { why: door?.why || "The home computer isn't on a Wi-Fi network phones can join." }));
      return;
    }
    // Phones may use the network the home computer is on now.
    if (!listed) send({ type: "add_door_network" });
    send({ type: "start_pairing", person });
  };

  const copy = async () => {
    if (!pairing) return;
    try {
      await navigator.clipboard.writeText(pairing.link);
      toast("Link copied. Send it to the phone some private way, e.g. AirDrop.");
    } catch {
      toast("Couldn't copy the link.", true);
    }
  };

  const mins = live ? Math.max(0, Math.ceil((live.pairing.expires_ms - live.now) / 60000)) : 0;
  return (
    <>
      <div className="btn-row" id="addDeviceRow" hidden={!farm || formOpen || !!pairing}>
        <Button id="addDevice" icon={<PlusIcon />} onClick={() => dispatch(local("activity/pairForm", { open: true }))}>
          Add a device
        </Button>
      </div>
      <form className="add-site" id="pairForm" hidden={!formOpen || !!pairing} onSubmit={submit}>
        <Input
          id="pairPerson"
          ref={personRef}
          type="text"
          list="pairPeople"
          autoComplete="off"
          spellCheck={false}
          placeholder={you ? `Who it's for, e.g. ${you}` : "Who it's for"}
          aria-label="Who the device is for"
        />
        <datalist id="pairPeople">
          {people.map((p) => (
            <option key={p.name} value={p.name} />
          ))}
        </datalist>
        <Button kind="primary" type="submit">
          Show pairing code
        </Button>
        <Button kind="quiet" id="pairFormCancel" onClick={() => dispatch(local("activity/pairForm", { open: false }))}>
          Cancel
        </Button>
      </form>
      <Banner tone="warn" id="pairWhy" hidden={!why}>
        {why}
      </Banner>
      <div className="pairing" id="pairBox" hidden={!pairing}>
        <div
          className="qr"
          id="pairQr"
          aria-label="QR code to scan with the oxbot app on the phone"
          // oxbot's own drawing of the code, not anyone's text.
          dangerouslySetInnerHTML={{ __html: pairing ? pairing.qr_svg.replace(/^<\?xml[^>]*>/, "") : "" }}
        />
        <div className="pair-text">
          <p>
            In the oxbot app on the phone, tap <b>Scan the code</b> and point it here.
          </p>
          <p className="code" id="pairCode">
            {pairing?.code}
          </p>
          <p className="field-hint" id="pairWhen">
            {pairing ? `For ${pairing.person}. Works once, for ${mins} more minute${mins === 1 ? "" : "s"}.` : ""}
          </p>
          <div className="btn-row">
            <Button size="sm" id="pairCopy" onClick={copy}>
              Copy link
            </Button>
            <Button kind="quiet" size="sm" id="pairCancel" onClick={() => send({ type: "cancel_pairing" })}>
              Cancel
            </Button>
          </div>
        </div>
      </div>
    </>
  );
}

function PeopleRows() {
  const { send, dispatch } = useApp();
  const settings = useSel((s) => s.connection.settings);
  const people = useSel((s) => s.home.people);
  const gates = useSel((s) => s.home.gates);
  const personOpen = useSel((s) => s.activity.personOpen);
  const arm = useArm();
  const farm = !!settings?.farm;
  const you = settings?.you;

  // Everyone, and anyone with a device who isn't listed yet.
  const list: Person[] = [...people];
  for (const g of gates)
    if (!list.some((p) => p.name === g.person))
      list.push({ name: g.person, color: people.find((p) => p.name === g.person)?.color || "var(--muted)", role: "adult", photo: null });

  return (
    <List className="people-rows" id="peopleRows">
      {list.map((p) => {
        const devs = gates.filter((g) => g.person === p.name);
        const role = p.role === "owner" ? "owner" : p.role === "kid" ? "kid account" : "";
        const devText =
          [p.role === "owner" ? "this Mac" : "", ...devs.map((g) => `${g.name}, ${seenText(g)}`)].filter(Boolean).join(" · ") || "No devices yet";
        const mine = p.name === you;
        const canOpen = mine || (farm && devs.length > 0);
        const isOpen = personOpen === p.name;
        return (
          <ListRow
            key={p.name}
            className="person-row"
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
              canOpen ? (
                <Button
                  kind="quiet"
                  size="sm"
                  aria-expanded={isOpen}
                  onClick={() => {
                    if (!isOpen && mine) send({ type: "get_profile" });
                    dispatch(local("activity/person", { name: p.name }));
                  }}
                >
                  {isOpen ? "Done" : mine ? "Edit" : "Devices"}
                </Button>
              ) : null
            }
          >
            {isOpen ? (
              <div className="person-edit">
                {farm && devs.length ? (
                  <List className="device-list">
                    {devs.map((g) => (
                      <DeviceRow key={g.id} g={g} armed={arm.armed === g.id} onRemove={() => arm.fire(g.id) && send({ type: "remove_gate", id: g.id })} />
                    ))}
                  </List>
                ) : null}
                {mine ? <MeForm /> : null}
              </div>
            ) : null}
          </ListRow>
        );
      })}
    </List>
  );
}

function DeviceRow({ g, armed, onRemove }: { g: GateInfo; armed: boolean; onRemove: () => void }) {
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
