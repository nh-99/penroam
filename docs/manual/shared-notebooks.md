# Shared notebooks

[Manual](README.md) › Shared notebooks

> **Availability.** Shared notebooks need a sync server that offers them. Penroam Cloud doesn't
> offer sharing yet; it will be turned on after testing. Until then, **Share notebook…** only
> explains that the server doesn't support it, and the buttons for people, comments, activity and
> notifications don't appear.

You can share a notebook with other people, so that they can read it or write in it too. Sharing
needs [sync](sync-and-vaults.md#sync): you and the people you invite sign in to Penroam Cloud, and
the notebook stays up to date on everyone's devices.

Read [Privacy](#privacy) before you share. Shared notebooks are not end-to-end encrypted.

## Sharing a notebook

1. In the library, open the notebook's menu and choose **Share notebook…**. In the editor, choose the
   people icon in the top bar.
2. Read the privacy notes and choose **Start sharing**. Nobody is invited yet.
3. Type the email address of the person, pick a role and choose **Send invitation**.

The notebook keeps its title, pages and version history. It gets a **Shared** line on its card.

If **Share notebook…** only shows an explanation, one of these applies: the notebook is in a vault
(see [below](#notebooks-in-a-vault)), you haven't signed in to Penroam Cloud, or the server doesn't
support shared notebooks (which is the case for Penroam Cloud for now).

### Roles

| Role | Can |
|---|---|
| **Owner** (you) | Everything: write, invite people, change roles, remove people |
| **Can edit** | Read and write, add and remove pages, restore earlier versions, comment, and see who has access. Can leave |
| **Can comment** | Read, search and export, and take part in [comments](comments.md): start discussions, reply and resolve them. Can't change the notebook itself. Can leave |
| **View only** | Look, search, zoom and export, and read the comments without writing any. Can leave |

There is one owner. The owner can't leave or be removed.

### Inviting

An invitation goes to a verified email address: the address the person signs in with. Penroam does
not send an email, so tell them to open Penroam. The invitation is valid for 14 days. You can't see
whether an address has a Penroam account, and the answer looks the same either way.

Pending invitations are listed under **Waiting to accept**, where you can cancel them.

### Changing roles and removing people

In the share dialog, the owner can change a person's role with the menu next to their name, or
remove them with the remove button. A change applies to their next request, even if their device is
open. People who were removed stay listed as **Former member**, without an email address.

### Leaving

Anyone but the owner can open the share dialog (the people icon in the editor, or **People and
leaving…** on the card) and choose **Leave this notebook**. Nothing is deleted for anyone else. What
your device already has stays until you remove it.

## Accepting an invitation

Invitations show at the top of your library under **Invitations**. They say who invited you and what
you could do, but not the notebook's title; you see the title after you accept. Choose **Accept** or
**Decline**.

After accepting, the card shows **Joining…** while Penroam downloads and checks the notebook. It
then appears under **Shared with me**, with a chip for your role.

If accepting asks you to sign in again, do that: Penroam has to confirm your email address.

## What viewers and commenters can do

A notebook you can only view opens with a banner: *View only — you can look and export, not edit.*
If you can comment, the banner says *Comment only — you can read, comment and export, not edit.*
You can turn pages, zoom, pan, search and print. You can export it in every format, which makes a
copy of what you have. Drawing, typing, erasing, selecting, undo, the page list, templates and PDF
import are turned off, and the keyboard shortcuts for them do nothing.

## Working at the same time

When other people have the notebook open and are online, you work in it together.

* **Who is here.** Small round badges in the top bar show the other people in the notebook now:
  their initials, a shape and a colour, so they can be told apart without the colour. Point at a
  badge, or use a screen reader, to hear their name, what they can do, and which page they're on.
  A person Penroam can't name yet appears as **Collaborator**.
* **Where they are.** On your page you see a marker with their name where their text cursor is, a
  tinted, hatched area around text they have selected, and a dashed outline with *is looking here*
  around the part of the page they're viewing. When their exact position isn't known, the text block
  they're typing in is outlined instead. These markers don't move or flash, so nothing distracts
  you while you write, and they work the same with the ePaper theme.
* **Pen and ink.** Your pen is never slowed down by this. What others draw appears on your page
  once they've finished a stroke. It doesn't change your tool or interrupt a stroke you're drawing.
* **Live status.** Next to the sync cloud in the top bar, **Live** means the live connection is up.
  **Live disconnected — changes will sync when online** means it isn't: ordinary sync still works
  and nothing is lost, but you won't see others' changes as they happen. A person's marker alone
  never means your changes were saved; the **Saved** and sync indicators do.
* **Presence is optional.** In **Settings → Sync**, turn off **Share my presence in shared
  notebooks** and others stop seeing which page you're on and where your cursor is. You still see
  them, and everyone's edits still sync.

### Typing in the same block

Two people can type in the same text block at the same time. Their changes are merged into each
other's text as they type, and your cursor and selection stay where they were. While you're
composing text with an input method (for Chinese, Japanese, Korean and similar), changes from others
wait until you finish the word, so your composition is never cut off.

**Undo only undoes your own typing.** If a collaborator typed in the middle of what you wrote, undo
removes your words and leaves theirs, and if you deleted something, undo puts back only what you
deleted. This holds for undo and redo, in the text box and on the page.

If two people rewrite the whole same block at once, both new texts are kept, one after the other, and
you can delete the part you don't want; see [Known issues](../known-issues.md).

## Working offline and conflicts

You can read and write a shared notebook without a connection. Your changes are saved on the device
and sent when you're back online. If two people change the same thing at the same time, Penroam
keeps both versions. The notebook then says **Needs resolution**. Choose **Resolve differences** on its card in the
library, or the **Needs resolution** label at the top of the editor, to open the conflicts dialog
and pick the version to keep. See [Sync and vaults](sync-and-vaults.md).

## What the labels mean

Shared notebooks show where your device stands, on the library card and in the editor's top bar.

| Label | Meaning |
|---|---|
| **Needs catch-up** | This device was away long enough that Penroam is rebuilding the notebook from the server's latest checkpoint. Changes not sent yet are kept and go up afterwards |
| **Joining…** | You accepted; the notebook isn't on this device yet |
| **Local saved** | Saved on this device; not sent yet |
| **Waiting to upload** | Changes are queued to be sent |
| **Synced** | Your copy matches the server's |
| **Needs resolution** | Two versions of something conflict; choose **Resolve differences** to pick one |
| **Access removed** | You are no longer a member |

## When your access is removed

If the owner removes you, the notebook stays on your device so you don't lose anything. It shows
**Access removed**, opens view-only, and nothing you change is sent. Under **Shared with me** you can:

* **Keep a private copy**: makes your own notebook in your library, including edits you hadn't sent.
  It's no longer connected to the shared one.
* **Remove from this device**: deletes only this device's copy. Nothing is deleted for the owner or
  anyone else.

If you are invited to the same notebook again, Penroam asks you to deal with the old copy first
(keep it as a private copy or remove it), and then you can accept.

## Notebooks in a vault

A notebook in a [vault](sync-and-vaults.md#vaults) can't be shared, because a vault is private to
you. Shared notebooks and vaults are separate.

## Privacy

* **Penroam Cloud can read shared notebooks.** Shared notebooks are like ordinary synced notebooks,
  not end-to-end encrypted. Only [vaults](sync-and-vaults.md#vaults) are private from the server.
  This covers [comments](comments.md) and the live markers too: they're encrypted on the way, but the
  service holds the key, like it does for the notebook itself.
* **The service records who changed what.** For every change in a shared notebook it keeps who
  sent it, when, and from which device. That's what [Activity](history-and-activity.md) shows to the
  people in the notebook.
* **Sharing an existing notebook shares its earlier versions too**, not only new changes. People can
  look through its version history.
* **Removing someone doesn't take back what they have.** They keep any copy already downloaded or
  exported, and nothing can erase it. Removal only stops future updates reaching them.
* **The owner's storage pays for it.** A shared notebook, including everything editors upload, counts
  against the owner's allowance. Uploads an editor made stay counted even after you remove that
  editor; removing them doesn't free the space. If the allowance is full, edits to the shared
  notebook and to the owner's own notebooks stop uploading until space is freed or the plan is
  raised. They stay saved on each device, and reading and exporting still work.
* **Email addresses.** People in the same shared notebook can see each other's verified email
  address. Nobody else can. See [Privacy](../policies/privacy.md).

## Limitations

* Invitations only appear in the app. No email is sent, and neither are notifications for
  [comments](comments.md#notifications): they appear in the app only.
* Every device that uses a shared notebook needs an up-to-date Penroam. Once you share a notebook,
  an older version stops syncing and says it needs an update (and can't edit text in it at all). A downgraded app might show a shared
  notebook as an ordinary one. Update instead of downgrading.
* Ownership can't be transferred yet.
* A viewer's copy of an exported notebook is an ordinary notebook that isn't connected to the
  shared one.

More: [Comments and mentions](comments.md), [History and activity](history-and-activity.md).
