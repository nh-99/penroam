# History and activity

[Manual](README.md) › History and activity

> **Availability.** Shared notebooks need a sync server that offers them. Penroam Cloud does; on a
> server that doesn't, **Share notebook…** only explains that, and the buttons for people,
> comments, activity and notifications don't appear.

In a [shared notebook](shared-notebooks.md), Penroam shows what changed and who changed it, and
lets you bring back earlier work without throwing away what others did since.

## Activity

Choose the activity button in the editor's top bar (shared notebooks only). It lists changes
newest first, like *Added 3 strokes · page 2*, with who made them and when. Choose a change to go to
its page.

* **Filter** by page, by person, and by kind of change (strokes, text, pages, paper, PDF, renames,
  restores, and so on).
* Use the **arrow keys**, **Home** and **End** to move through the list.
* **Show older changes** loads more.

Summaries come from the kind and number of changes only. Penroam doesn't try to describe what you
wrote or drew.

### Who made a change

A name is the person's verified email address, with initials and a shape beside it, so it never
depends on colour. Some changes can't name a person, and Penroam says so instead of guessing:

| Label | Meaning |
|---|---|
| **Before sharing** | Made before the notebook was shared. Earlier changes have no recorded author |
| **Former member** | Made by someone who has since been removed or left |
| **Former collaborator** | Made by an account that has since been deleted |
| **Unverified author** | The service's record and the device that sent the change don't match |
| **Unknown** | Nothing says who made it |
| **Edited offline** | Not an author: the change was made without a connection and sent later. Both times are shown |

The author comes from the service's record of who sent each change, not from the device that
passed it on.

## Version history

The clock in the top bar opens the page's earlier versions, as in
[Saving, backups and version history](backups-and-history.md#version-history). Pick a version to
see it as a picture. It's only a picture: you can't draw on it, and the pen isn't active while it's
open. In a shared notebook, each version in the list says **By** whom, using the same labels as
Activity (a version covering several writing sessions lists everyone who made changes in it).

### Comparing versions

With a version chosen, use **Compare with** to pick the current page or another version. The preview
marks what differs, with a symbol as well as an outline: **+** (solid) for added, **−** (dashed) for
removed, **~** (dotted) for changed. Text changes show the words that were added underlined and the
words that were removed struck through. You also get the counts of each, and whether the paper
changed.

## Restoring

Choose **Restore this page…** or **Restore whole notebook…**, and Penroam first shows what it would
do. Nothing changes until you tick what you want and choose **Restore**.

**In a shared notebook**, a restore never replaces the page with an old copy and never removes what
others did since:

* Things deleted since then can be **brought back**.
* Things changed since then can be **put back the way they were**. Those that only you changed are
  ticked. Those someone else changed are unticked, with their name, and a note warns that restoring
  one replaces their work.
* For text others changed, you can instead **restore the old text as a recovered copy** next to the
  current one. For each block, choose one of the two, not both.
* Anything added since then **stays**; the preview says how many additions stay.
* For the whole notebook, deleted pages come back, pages made since are kept, and the title can go
  back too.

**In a notebook that isn't shared**, a restore returns it exactly to the earlier version, and the
list is ticked that way: things changed since are put back, **things added since are removed**
("Remove 3 things added since then"), the paper goes back, and for the whole notebook, each page's
content comes back ("Bring back page 2's content (4 changes)"), **pages made since move to the
trash**, the pages go back in their old order, and the title returns. Untick any of these to keep
that part as it is. Later changes go to the trash or are removed, and you can undo the restore by
restoring again from the history.

Anything from then that can't be brought back is listed as such.

It's one new edit: it syncs, shows in Activity, and can be undone from the history. If the page
changed while you were looking (or the preview was open for a long time), the list is made again and
you're asked to check it again.

**Pick elements…** lets you restore only some things: tap the boxes over the preview, then **Restore N
chosen…**, where N is how many you picked.

Only the owner and people who can edit can restore a shared notebook.

## Conflicts

When two people change the same thing at once, both versions are kept and the notebook says **Needs
resolution**. Open the conflicts dialog to see both versions, who made each (a version no one is recorded for
shows **Unknown**), and choose:

* **Keep this version**: it replaces the other.
* **Keep both as copies**: the version shown stays, and the other becomes a separate copy next to
  it, so nothing is lost.

A choice is an ordinary edit and syncs to everyone.

## Exporting with authors

Exporting a shared notebook as `.penroam` also asks about **Include authors**, next to **Include
comments**. Both are off by default. With authors included, the file records who wrote each change by
email address, which can't be taken back from anyone who has the file. Without it, the file holds
the notebook only.
