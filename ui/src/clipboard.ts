// Copying text for the person: the clipboard where the browser lets the page, else nothing (the
// caller says to select it by hand).

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}
