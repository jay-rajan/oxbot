// oxbot's Terms of use and Privacy policy, as the app shows them: in
// Settings → Terms & privacy, and on the owner's welcome. One text, so both
// say the same. The store listings link to the same policy on the web.
import "./legal.css";

/** When this version took effect. */
export const LEGAL_DATE = "November 20, 2026";
/** Where to ask about them. */
export const CONTACT = "github.com/jay-rajan/oxbot/issues";

export function TermsText() {
  return (
    <div className="legal-text" id="termsText">
      <h3>Terms of use</h3>
      <p className="legal-date">Effective {LEGAL_DATE}</p>

      <h4>1. What oxbot is</h4>
      <p>
        oxbot runs on your Mac, the home computer. Its bots answer questions and browse the web for the people in your home. Phones and other devices pair with the home computer
        and connect only to it. The publisher of oxbot runs no servers that receive your data.
      </p>

      <h4>2. Who may use it</h4>
      <ul>
        <li>The owner, who installs oxbot and connects the home's OpenRouter account, must be 18 or older and accepts OpenRouter's terms (openrouter.ai/terms).</li>
        <li>Other people in the home use it with the owner's permission. The owner and the administrators are responsible for them.</li>
        <li>Anyone under 18 may use oxbot only with a kid account (section 3), set up and supervised by an administrator.</li>
      </ul>

      <h4>3. Kid accounts (under 18)</h4>
      <p>An administrator can mark someone in the home as under 18 in Settings → People. For that person:</p>
      <ul>
        <li>Every message is checked by AI before a bot sees it. A message a parent should see first waits for an administrator to allow it or say no.</li>
        <li>Every photo and file they send waits for an administrator's approval before a bot can see it.</li>
        <li>Every request from their chats that needs a yes (a click on a website, a schedule, a script) goes to the administrators, never to them.</li>
        <li>Bots are told to answer them as a child: kindly, simply and suited to their age.</li>
      </ul>
      <p>
        <b>Their messages go to AI models through the home's OpenRouter account, like everyone else's.</b> OpenRouter's terms require its users to be 18 or older. The home's account belongs to
        the owner, and the owner and administrators are responsible for supervising a kid's use of oxbot and for deciding that it's right for them.
      </p>
      <p>These checks lower the risk but aren't perfect: AI can miss things or get them wrong. An adult should supervise. Don't let anyone under 18 use oxbot without a kid account.</p>

      <h4>4. Your AI account and costs</h4>
      <p>
        oxbot uses one OpenRouter account per home. You pay OpenRouter directly for any paid models; oxbot charges nothing. You're responsible for all use and charges on that account,
        including use by people in your home.
      </p>

      <h4>5. What bots may not be used for</h4>
      <ul>
        <li>Getting around a website's bot checks, CAPTCHAs or rules on automated access.</li>
        <li>Signing in to accounts that aren't yours, or using someone else's account without permission.</li>
        <li>Collecting personal information about other people without their consent.</li>
        <li>Breaking the law, harassing or harming anyone, including children, or breaking OpenRouter's or a model's terms.</li>
      </ul>
      <p>oxbot asks before a bot changes something on a website (sending, posting, buying, booking, deleting, uploading). Paying, booking, deleting and uploads always ask.</p>

      <h4>6. AI answers can be wrong</h4>
      <p>Bots make mistakes. Check anything important (prices, dates, health, money, legal or safety matters) before relying on it. oxbot isn't professional advice.</p>

      <h4>7. Open source, no warranty</h4>
      <p>
        oxbot is open source under the MIT or Apache 2.0 license, at your choice, and includes other open-source software under its own licenses. It's provided "as is", without
        warranties, to the extent the law allows. The publisher isn't liable for what bots do on websites you let them use, for charges on your OpenRouter account, or for how third
        parties handle your data, to the extent the law allows. Nothing here limits rights you have under consumer law.
      </p>

      <h4>8. Changes and contact</h4>
      <p>New versions of these terms come with oxbot updates. Questions: {CONTACT}.</p>
    </div>
  );
}

export function PrivacyText() {
  return (
    <div className="legal-text" id="privacyText">
      <h3>Privacy policy</h3>
      <p className="legal-date">Effective {LEGAL_DATE}</p>
      <p>
        <b>In short:</b> oxbot runs on your own Mac. The publisher has no servers that receive your data, and the apps have no ads, analytics or tracking. The phone apps talk only to your
        Mac.
      </p>

      <h4>What stays on your Mac</h4>
      <p>
        Chats, bot settings, schedules, the family Calendar, Groceries, Meal Plan and Records; files and photos you send to bots; websites' sign-ins and cookies in oxbot's own browser;
        usage records; and your AI account key, in the Mac's Keychain. None of it is sent to the publisher.
      </p>

      <h4>What leaves your Mac</h4>
      <ul>
        <li>Your request to a bot, the parts of web pages it reads, and its instructions go to OpenRouter, through your home's own account, and on to the AI model it uses. Their privacy policies apply to what they receive.</li>
        <li>Ordinary web traffic goes to the websites your bots visit.</li>
        <li>A check for new versions goes to GitHub.</li>
      </ul>

      <h4>The phone apps</h4>
      <ul>
        <li>Send data only to your paired Mac.</li>
        <li>Use the camera to scan the pairing code, and to take a photo only when you choose to send one.</li>
        <li>Send only the photos and files you pick.</li>
        <li>Keep a copy of recent chats and your family's records so they open quickly. Unpairing the phone deletes them.</li>
        <li>Don't collect location, contacts, advertising identifiers, analytics or crash reports.</li>
      </ul>

      <h4>Kid accounts</h4>
      <p>
        For someone marked under 18, every message is checked by AI before a bot sees it, and messages that need a parent and every photo or file wait for an administrator. Their messages
        go to AI models through the home's OpenRouter account like everyone else's, and are kept on the Mac like everyone else's. Administrators can see a kid's chats and approve
        what they send. The publisher collects nothing from them.
      </p>

      <h4>Deleting your data</h4>
      <p>
        Deleting a bot removes its chats. Uninstalling oxbot and deleting its data folder removes everything on the Mac. Unpairing a phone removes what it kept. Data that already reached
        OpenRouter or a model provider follows their retention rules.
      </p>

      <h4>Changes and contact</h4>
      <p>New versions of this policy come with oxbot updates. Questions: {CONTACT}.</p>
    </div>
  );
}
