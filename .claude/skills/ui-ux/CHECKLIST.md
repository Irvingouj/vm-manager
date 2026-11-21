# The review checklist

Ask these of every page, in this order, at 320, 768 and 1280 px, light and dark, looking at a screenshot. Write down each "no" with the page and the rule's number. The reasons, with sources, are in `docs/research/refactoring-ui-and-tailwind-defaults.md` (RU) and `docs/research/hci-principles-for-the-platforms-pages.md` (HCI).

## A. Orientation: where am I, what can I do, what just happened

1. Does the page say what it is in its title and its one `h1`, in words the person would use, the page's own part first in the `<title>`? (Nielsen: match with the real world; WCAG 2.4.2)
2. Can the person tell they are signed in, as whom, and how to sign out, without the identifier shouting? (visibility of system status)
3. Is there always a way back to the Dashboard and a way out of a dead end? (user control and freedom)
4. After an action (shared, removed, signed out), does the page say what happened, near where it happened? (feedback; gulf of evaluation)
5. Is the same thing called the same name and found in the same place on every page: the vocabulary of `CONTEXT.md`, the header, the buttons' order? (consistency; WCAG 3.2.3, 3.2.4)
6. Does it behave as other sites do: the mark links home, links are underlined or obviously links, the primary button is on the left of its row or at the end of its form? (Jakob's law)

## B. Hierarchy and action

7. Is there exactly one primary action in view, and is it the only filled accent button? (RU: action hierarchy; Von Restorff)
8. Are the other actions clearly lesser: outlined or quiet? Is a destructive action quiet unless confirming it is the page's one job?
9. Does the eye land first on what matters (the Application's name, the message's first sentence), by weight and colour rather than size alone?
10. Is secondary information de-emphasised: identifiers, Versions, timestamps, labels smaller and softer than what they describe?
11. Are choices few? More than about five options at one level are grouped or progressively disclosed. (Hick's law)
12. Does the page still read correctly in greyscale?

## C. Grouping and space

13. Is the gap between groups clearly larger than the gap inside a group: label to field < field to field < section to section? (Gestalt proximity; RU: ambiguous spacing)
14. Do things that are alike look alike, and only those? (similarity)
15. Is every enclosure earning its place: could spacing or a background do what this border does? No box inside a box inside a box. (common region; RU: fewer borders)
16. Does every space and size come from the scale in THEME.md? No stray `13px`.
17. Is there enough white space that nothing touches or crowds, and is content held to a `max-width` instead of stretching?
18. Are edges aligned: one left edge for a card's content, baselines shared across a row?

## D. Text

19. Do sizes come from the type scale with their paired line heights, with only two weights?
20. Is running text at most about 65 characters wide?
21. Is every sentence plain, short and front-loaded, in sentence case, saying the thing before the detail?
22. Are identifiers, addresses and long names monospaced where they are codes, wrapped instead of overflowing, and never louder than a name?
23. Are numbers that are compared aligned and tabular?

## E. Colour, contrast, depth

24. Does every colour come from THEME.md? Do `tokens-light.txt` and `tokens-dark.txt` pass `scripts/contrast.py --tokens`?
25. Text 4.5:1; large text, control edges and focus rings 3:1; in both schemes. (WCAG 1.4.3, 1.4.11)
26. Is meaning never carried by colour alone: status has words and a mark, links in text are underlined? (WCAG 1.4.1)
27. Is the accent used only for the primary action, links, focus and the current item?
28. In dark, do surfaces get lighter as they come forward, with hairlines instead of shadows, and no pure black or pure white?
29. Do shadows come from the set, fall downward, and match importance?

## F. Controls, forms, errors

30. Is every target at least 24 by 24 CSS px, the main buttons and fields 44 px tall, with 8 px or more between neighbours? (WCAG 2.5.8; Fitts's law)
31. Does every field have a visible label above it, and is nothing a placeholder-only label? (WCAG 3.3.2)
32. Does each button say what it does with a verb and its object ("Share hello", "Remove Grace Hopper", "Sign out")?
33. Is an error shown next to its field and, on a long form, summarised at the top; does it say what happened and how to fix it, without blame or codes? (WCAG 3.3.1, 3.3.3; Nielsen: help users recover)
34. Is what the person typed kept after an error? (WCAG 3.3.7; Postel's law: accept what can be understood)
35. Are slips prevented: a destructive action names its object, and a wrong click can be undone or is confirmed? (Norman: design for error; Nielsen: error prevention)
36. Is nothing disabled without saying why, and is nothing interactive that does not look it, or the reverse? (signifiers)
37. Is a link used for going somewhere and a button for changing something (sign-out included: a form's button, not a link)? Is an optional field marked "(optional)", with no asterisks?

## G. Keyboard, zoom, narrow, motion

38. With the keyboard alone: can everything be reached in the page's order, and is focus always visible with an `outline`, 3:1 against its surroundings, in both schemes? (WCAG 2.1.1, 2.4.3, 2.4.7, 2.4.11)
39. At 320 CSS px: no sideways scroll, nothing clipped, nothing overlapping, the header no taller than it must be? (WCAG 1.4.10)
40. At 200 % text size and with the text-spacing overrides, does nothing clip or overlap? (WCAG 1.4.4, 1.4.12)
41. Does nothing depend on hover, and nothing move on its own; are transitions off under `prefers-reduced-motion`?
42. Is `lang` set, is there one `h1`, do headings descend in order, and does every control have a name a screen reader can say? (WCAG 3.1.1, 1.3.1, 2.4.6, 4.1.2)

## H. Empty, slow, wrong

43. Does an empty list say what would be here and how to get the first one?
44. Does a refusal or error page say what happened, why, and the one thing to do next, with a way onward, and is no status code or internal identifier its main message?
45. Does the page read as the same product as the website and every other page: same shell, same type, same buttons?

## The cognitive walkthrough (for each task)

For each step of a task (share an Application with a person; remove them; sign out; arrive at a refusal and recover): (1) will the person be trying to do the right thing? (2) will they see that the control is there? (3) will they connect that control with what they want? (4) after acting, will they see that it worked? A "no" is a finding.
