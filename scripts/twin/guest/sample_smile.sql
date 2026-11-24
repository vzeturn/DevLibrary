-- Synthetic SMILE-shaped sample databases for the twin. Invented rows only: no real customer
-- data, and no SQL login is created or altered here (smile/smilepms keep their passwords).
-- Extend the tables as the modules' schema probes need; keep every value fictional.
IF DB_ID(N'SMILE_TWIN') IS NULL CREATE DATABASE [SMILE_TWIN];
GO
USE [SMILE_TWIN];
GO
IF OBJECT_ID(N'dbo.Rooms') IS NULL
  CREATE TABLE dbo.Rooms (RoomNo nvarchar(10) NOT NULL PRIMARY KEY, RoomType nvarchar(20) NOT NULL, Rate decimal(18,2) NOT NULL);
IF OBJECT_ID(N'dbo.Guests') IS NULL
  CREATE TABLE dbo.Guests (GuestId int IDENTITY(1,1) PRIMARY KEY, FullName nvarchar(80) NOT NULL, ArrivedOn date NULL);
GO
IF NOT EXISTS (SELECT 1 FROM dbo.Rooms)
  INSERT dbo.Rooms (RoomNo, RoomType, Rate) VALUES (N'101', N'STD', 500000), (N'102', N'STD', 500000), (N'201', N'DLX', 800000);
IF NOT EXISTS (SELECT 1 FROM dbo.Guests)
  INSERT dbo.Guests (FullName, ArrivedOn) VALUES (N'Test Guest One', '2026-01-01'), (N'Test Guest Two', '2026-01-02');
GO
