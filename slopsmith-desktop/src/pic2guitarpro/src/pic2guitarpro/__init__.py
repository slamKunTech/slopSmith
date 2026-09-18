"""pic2guitarpro — convert guitar tab image collections into Guitar Pro scores.

Pipeline:
    tab images (per song, Roman-numeral ordered)
      -> OCR (ocr_tabber) -> ASCII tab
      -> structured notes (ascii_parser)
      -> PyGuitarPro Song model (song_builder)
      -> .gp5 file in output/
"""

__version__ = "0.1.0"
