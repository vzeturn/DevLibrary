IF OBJECT_ID('dbo.TransactionCode') IS NOT NULL DROP TABLE dbo.TransactionCode;
CREATE TABLE dbo.TransactionCode (TransactionCode varchar(10), TransactionSubCode varchar(10), Description nvarchar(100));
INSERT dbo.TransactionCode VALUES
 ('110','0','Room'), ('110','1','Room Plus'), ('120','00','Food'),('120','',''),
 (' 150 ','007','Padded'), ('160','2','Dup A'), ('160','02','Dup B'), ('170','','Blank sub'), ('180','1','Only sub1'),('190','AB','Text sub'),(NULL,'1','Null code');
