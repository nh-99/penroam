# Comments and mentions

[Manual](README.md) › Comments and mentions

> **Availability.** Shared notebooks need a sync server that offers them. Penroam Cloud doesn't
> offer sharing yet; it will be turned on after testing. Until then, **Share notebook…** only
> explains that the server doesn't support it, and the buttons for people, comments, activity and
> notifications don't appear.

In a [shared notebook](shared-notebooks.md) you can leave comments on the page, reply to each other
and mention people. Comments are a separate layer: they don't change the notebook's pages, and
they don't appear in a PDF you export.

Who can write comments: the owner, people who **Can edit**, and people who **Can comment**. People
with **View only** can read them.

## Opening the comments

Choose the speech bubble in the editor's top bar. A number on it counts discussions with something
new. On a wide screen the comments open as a panel on the right; on a phone they open as a sheet from
the bottom. Notebooks that aren't shared have no comments.

Small numbered markers on the page show where discussions are attached. Tap one to open its
discussion. A marker with ✓ is resolved, one with … is waiting to send, and one with ! failed to
send. Markers stay in the same place on the page when you zoom or move, and never animate.

## Adding a comment

In the panel, under **Add a comment on**, choose what the comment is about:

* **Selected object**: a stroke, picture or text block you've selected (not available while nothing
  is selected, or if you can only comment).
* **Pick an object**: then tap the drawing, text or picture on the page.
* **Selected text**: select some words in a text box first, then choose this. The comment follows
  those words if the text around them changes.
* **A point**: then tap the page where the comment belongs.
* **An area**: then drag over the part of the page. A single tap makes a small box.
* **PDF area**: on a page with a PDF behind it, drag over the part of the PDF.

Choosing doesn't change your pen: the pen, eraser or text tool you had stays selected, and works
again as soon as you've chosen. Press **Escape** or **Cancel** to give up. Then write the comment and
choose **Add comment**.

## Replying, editing and deleting

Open a discussion to read it and choose **Reply**. You can **Edit** your own comments, and they're
then marked *(edited)*. **Delete** hides your own comment: it shows as *Deleted*. It isn't erased:
the text stays in the notebook's comment history and on the server. The owner can **Remove**
someone else's comment, which shows as *Removed by the owner*.

**Resolve** closes a discussion and **Reopen** opens it again. A reply to a resolved discussion
reopens it.

## Mentions

Type **@** in the box to see the people in this notebook, and only them. Pick one with the arrow
keys and **Enter**, or by tapping, and their name appears as a chip under the box. **Escape** closes
the list. The people you mention get a notification. If someone has since left the notebook, their
mention shows as *Former member*.

Links starting with `http://` or `https://` can be opened. If a link's text hides its real address
(for example, an international look-alike of a well-known site), the real address is shown in
brackets after it. Other kinds of link are plain text, and nothing in a comment is ever treated as
formatting or code.

A comment can hold about 8 KB of text (fewer characters for Chinese, Japanese or Korean).

## Finding comments

At the top of the panel, choose **Open**, **Resolved**, **Mine** or **Mentions**. A dot, with the
words *New activity* for screen readers, marks discussions you haven't seen.

### Comments that lost their place

If what a comment was attached to is deleted, or the words it was attached to changed, it moves to
**Not on the page**. It never jumps to something else. Choose **Locate** to go to the page it was
last on, or **Re-attach…** and drag an area to give it a new place.

## Without a connection

You can comment offline. The comment stays on your device with **Waiting to send**, and goes when
you're online again. If sending failed it says **Failed — retry** with a **Retry** button. If you
lose the right to comment in the meantime, an unsent comment becomes **Draft (you can no longer
comment)** and is kept on your device; it isn't sent. A comment that is waiting is never shown as
sent.

## Notifications

The bell in the library and the editor top bar shows how many notifications are unread. It lists
people who mentioned you or replied in a discussion you're in. Choose one to open the notebook and
the discussion. If that notebook isn't on this device (yet), Penroam says so and shows nothing about
it.

Notifications only appear in the app; no email is sent. Choose **Mute thread** or **Mute notebook**
on a notification (or **Mute thread** in a discussion) to stop them. Muting works only for you.

## Exporting with comments

Exporting a shared notebook as a `.penroam` file asks whether to **Include comments**. It's off by
default: the file would contain your collaborators' comment text and their account identifiers, and
anyone you give the file to can read them. That can't be taken back. See also
[History and activity](history-and-activity.md#exporting-with-authors).

## Privacy

Penroam Cloud can read comments, like the rest of a shared notebook. Notifications carry no text:
only which notebook, which discussion, who, and when. See [Privacy](../policies/privacy.md).
