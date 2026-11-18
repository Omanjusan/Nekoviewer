# Quick Reference

[Back to README](README.md) | [日本語版](QUICKREFERENCE.ja.md)

---

## First, know this: collecting fingerprints and thumbnails

Walk through the folders that hold your image archives so the app can collect fingerprints and thumbnails. The app never scans paths on its own.

- Doing this widens your search scope. Tag search in particular only covers files that already have a fingerprint.
- Conversely, folders that have nothing to do with images are never touched.

---

## Steps to get the most out of tag search

1. **Expand the tag UI**
   Near the top right of the explorer's card view or list view there is a triangle button. It expands and collapses the tag UI. If the UI is collapsed, press this button to expand it.
2. **Run learning**
   Use the buttons at the top of the tag UI and press the learning button for tag extraction ("Tag Learning"). It works best when run in a folder that holds CBZ files. If the displayed results contain mistakes, correct them by hand (this is basically a one-time job).
3. **Fetch tags automatically → Confirm**
   From the same UI, press the automatic tag fetch button. It has a different color, so you should spot it right away. When it finishes, left-click a few files here and there and check that the tags were applied correctly. If everything looks fine, press the "Confirm" button at the top right to declare the tagging official. From then on, the folder you ran it on is included in tag search. A somewhat complicated UI will appear; if it makes no sense to you, just go from page 1 to page 2 without changing anything and press the finish button — that is enough.
4. **Search**
   Select the "Tag Search" tab on the left and search by any tag name you like.

---

## Folders tab vs. Virtual Folders tab

### Folders tab

This works almost like your OS's file explorer or file manager: you reach your archive library folders through a folder tree.

- **Pro:** If you understand your OS's usual folder tree, you can use it without getting lost.
- **Con:** It also shows folders the viewer doesn't need, so it is hard to stay focused from a viewer's point of view.

### Virtual Folders tab

This tab removes the "con" above. First, at the virtual folder root (`/`), use "Add Virtual Folder" and add a real folder that corresponds to your archive library. From then on, you can reach two places: the root folder and the virtual folder you added. Repeat this to build the bookshelf you like.
Also, right-clicking in the virtual folder tree gives you various actions, so give them a try. Basically, no action has any destructive effect on real folders. (You can break the virtual folder layout information, but the design never touches real folders or files.)

- **Pro:** Favorites are managed per file, whereas virtual folders are a per-folder bookshelf management feature. You can concentrate on viewing without visiting needless places.
- **Con:** It takes some effort to build virtual folders into something easy to use.

---

## Using the viewer

### Unusual default controls

Some default controls differ from other viewers.

| Action | Default |
|---|---|
| Toggle full screen | Middle mouse button |
| Toggle actual size ⇔ fit to window size | Double click |

You can reassign these to any operation you like using the Toolbox. See "Recommended Setup" in the README for the steps (turn a function into a button, then right-click the button cell and choose "Assign key").

### Jumping within an archive from the thumbnail bar

If the thumbnail bar is enabled in the settings, you can jump to a file from the bar. While the pointer is over the bar, scrolling moves the thumbnail list instead. The viewer has quite a few overlay parts, so the bar may be hard to reach, but placing it at the bottom of the screen makes it the easiest to access.
You can also hover over the upper-left part of the viewer to show the file entry list, and jump from there as well. This list is good for checking the sort order.

### Viewer window position lock

#### For Linux users

The window lock does not work on Wayland. Open the "Window Position" menu, tick the checkbox to start with XWayland ("Start with XWayland next time"), and restart. If XWayland causes problems, untick the same checkbox to return to Wayland (restart required).

#### How to use

This app uses a one-to-one window system: one explorer window and one viewer window. You can keep the window frame locked and still view any image fitted to the window size. In other words, it is designed to be friendly to high-resolution environments such as 4K/8K. The moment you open an image file, the viewer automatically returns to the configured window position and you can start viewing.
First save the return position with "Save" in the "Window Position" menu, then tick the "Apply" checkbox of that slot, and the setup is done. Then open any image file to try it. This can be assigned to a function key.
