# Commands Used to Create the Program

**Date:** November 7, 2026  
**LLM:** Claude 5 / Opus 5.5 avg (Chat Mode)  

*Note: Don't ask why I didn't do this with Claude Code working as an agent.*

---

## Prompt 1
> Examine the attached file.
> 
> It is an MP3 stream from a Shoutcast server, which contains in-stream metadata.
> 
> Write a Rust program that fixes this file so that the metadata is compliant to be played in an MP3 player.
> 
> CLI messages in French.

* **Result:** First version delivered just put the first metadata found as an ID3 tag. I wanted all the titles.

---

## Prompt 2
> Modify the program to store multiple track titles in the `id3_tag`, along with their respective timestamps.

* **Result:** 2nd version maybe worked, but I was unable to find the ID3 tags in a player. Inspecting the file with `strings` shows they are somewhere in there...

---

## Prompt 3
> Write a new version which, in addition to the corrected `.mp3` file, creates a properly named `.cue` index file containing the various tracks found and their timestamps.

* **Result:** It works, the .cue files are correct, and Foobar2000 can read them. The id3_tag function are still in the source, but I still don't now if it works (see Prompt 2).

* **Tests:**  Program tested on W10 with 100% success rate on 92 files (~1GB for 50 songs/titles per file)

---

## Prompt 4
> Write a description in `.md` format for this project and upload it to GitHub.
> 
> Note that it was developed using Vibecode with Claude.

* **Result:** I changed some parts of the `.md` file by hand, like a caveman :)
